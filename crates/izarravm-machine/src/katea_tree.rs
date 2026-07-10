//! Recursive, read-only, lazy host-folder directory tree for the Katea
//! controller. Generalizes the flat `KateaVolume` into a full FAT32
//! directory tree whose FAT and directory sectors are computed on demand, so
//! RAM scales with the entry count rather than the disk or file sizes.

// `KateaTreeVolume` is consumed by the ATA `HostFolder` backing (`ata.rs`) and
// `mount_hdd_folder` (`lib.rs`). A few
// items remain reachable only from this module's `#[cfg(test)]` tests; each
// carries a narrow `#[allow(dead_code)]` at its definition: `tree()` and the
// `tree` field it reads, and the free `dir_sector` (the per-volume read path
// inlines the same logic in `data_sector`). The `tree` field is also the seam
// the write engine reads at construction.

use crate::fat32::{FAT32_EOC, fat32_dir_entry, fat32_fsinfo_sector};
use crate::katea_names::NameTable;
use crate::katea_volume::{
    ATTR_ARCHIVE, BACKUP_BOOT_SECTOR, BACKUP_FSINFO_SECTOR, FAT0_MEDIA, FSINFO_SECTOR, FileSource,
    NUM_FATS, PART_START, PART_TYPE_FAT32_LBA, RESERVED_SECTORS, ROOT_CLUSTER, SECTOR,
    fat_size_sectors, lba_to_chs, sectors_per_cluster, stamp_fat32_bpb,
};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Cap recursion so a pathological tree (or an undetected loop) can't run away;
/// also roughly the depth DOS's 64-char path limit allows.
const MAX_DEPTH: usize = 32;

/// Floor on the data-cluster count so the synthesized partition is always a
/// valid, boot-tested FAT32. This is exactly the flat static disk's data-cluster
/// count: `(PART_SECTORS - used) / spc` for the proven-bootable 96256-sector
/// partition (`used = 32 + 2*741`). Flooring here means a small host folder
/// reproduces the known-good geometry (`part_sectors = 96256`, `fatsz = 741`)
/// instead of landing just under `sectors_per_cluster`'s FAT32 floor (66601
/// sectors), where it would panic. A larger folder grows past this floor.
const MIN_DATA_CLUSTERS: u32 = 94_742;

#[derive(Debug)]
pub(crate) struct TreeFile {
    pub name: [u8; 11],
    pub source: FileSource, // InMemory (system files) or HostFile (lazy)
    pub first_cluster: u32, // assigned after the tree is built
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
    /// This directory's host-filesystem path (root = the mounted folder). Used by
    /// the write engine to know where to materialize files created here.
    pub host_path: std::path::PathBuf,
}

#[derive(Debug)]
pub(crate) struct HostTree {
    pub root: TreeDir,
}

/// The Toka-DOS system binaries that the overlay places in a synthetic `C:\DOS`
/// folder rather than the boot-drive root, so the host-folder root isn't buried
/// under system files. This is the executable subset of the committed image's
/// `dos_files` (`scripts/build-freedos-hdd-image.py`). NOT here — and therefore
/// left at the root — are the boot/kernel-required files (KERNEL.SYS, CONFIG.SYS,
/// AUTOEXEC.BAT), LICENSE.TXT (the kernel signon points at `C:\LICENSE.TXT`), and
/// any user or runner file (which stays where it was dropped). HELLO.TXT is
/// excluded too: it is a data file, so a caller-supplied one stays at the root
/// where the guest expects it (the image's own demo HELLO.TXT is filtered out of
/// the overlay before it ever reaches here). Matched case-insensitively against the
/// overlay's 8.3 names.
const DOS_FOLDER_BINARIES: &[&str] = &[
    "COMMAND.COM",
    "TOKAMOUS.COM",
    "TOKAEMM.SYS",
    "GSWMODE.COM",
    "MOVE.EXE",
    "SORT.EXE",
    "MEM.EXE",
    "ATTRIB.EXE",
    "CHOICE.EXE",
    "MORE.EXE",
    "FIND.EXE",
    "LABEL.EXE",
    "DELTREE.COM",
    "XCOPY.EXE",
];

/// Build the tree from a host folder, overlaying the in-memory system files (so the
/// disk still boots). The Toka-DOS binaries land in a synthetic `C:\DOS` subdir;
/// KERNEL.SYS / CONFIG.SYS / AUTOEXEC.BAT / LICENSE.TXT and any user/runner file
/// stay at the root. Metadata only — never reads host file contents. Cluster fields
/// are zero here; cluster assignment happens after construction.
pub(crate) fn build_tree(root: &Path, system_files: &[(String, Vec<u8>)]) -> HostTree {
    let mut names = NameTable::new();
    let mut dir = TreeDir {
        host_path: root.to_path_buf(),
        ..TreeDir::default()
    };
    // The synthetic C:\DOS folder. Its files are all InMemory and covered by
    // `system_names`, so reconcile classifies them Skip and never materializes them:
    // this folder never touches the host filesystem unless the guest itself writes a
    // non-system file into C:\DOS.
    // A guest write into C:\DOS would try to materialize under root/DOS/<file>;
    // atomic_write holds it if that host directory is absent. That is sufficient
    // while this system folder remains read-only.
    let mut dos = TreeDir {
        host_path: root.join("DOS"),
        ..TreeDir::default()
    };

    // System files, with their canonical 8.3 names, split by placement.
    for (name, bytes) in system_files {
        let n = fold_literal_83(name);
        let file = TreeFile {
            name: n,
            source: FileSource::InMemory(bytes.clone()),
            first_cluster: 0,
            cluster_count: 0,
        };
        if DOS_FOLDER_BINARIES
            .iter()
            .any(|b| name.eq_ignore_ascii_case(b))
        {
            dos.files.push(file);
        } else {
            names.reserve(n); // root system files must not be shadowed by a host file
            dir.files.push(file);
        }
    }

    // Attach the C:\DOS folder (reserving its name at the root so a host subfolder
    // named DOS can't collide), but only when it actually holds binaries — an empty
    // overlay (e.g. a test with no system files) leaves the root free of a stray DOS.
    if !dos.files.is_empty() {
        let dos_name = fold_literal_83("DOS");
        names.reserve(dos_name);
        dir.subdirs.push(TreeSubdir {
            name: dos_name,
            dir: dos,
        });
    }

    walk_into(root, &mut dir, &mut names, 1);
    HostTree { root: dir }
}

// `walk_into` is both the root entry (names already holds the system
// reservations) and the per-subdirectory recursion; each subdirectory gets its
// own fresh `NameTable` at the call site.
fn walk_into(host: &Path, dir: &mut TreeDir, names: &mut NameTable, depth: usize) {
    if depth > MAX_DEPTH {
        // A too-deep folder is truncated rather than recursed forever; warn once
        // at the cap so the loss is not silent.
        eprintln!("katea: directory tree deeper than {MAX_DEPTH}; truncating");
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
            continue; // No symlink following, which also avoids loops.
        }
        let path = e.path();
        if ft.is_dir() {
            let name = names.add_host(&path, true);
            let mut child = TreeDir {
                host_path: path.clone(),
                ..TreeDir::default()
            };
            let mut child_names = NameTable::new(); // a fresh table per directory
            walk_into(&path, &mut child, &mut child_names, depth + 1);
            dir.subdirs.push(TreeSubdir { name, dir: child });
        } else if ft.is_file() {
            let Ok(md) = e.metadata() else { continue };
            // Skip any file too large for a FAT32 32-bit size/cluster span, exactly
            // as `KateaVolume::new` does (`katea_volume.rs`): a `>= 4 GiB` file
            // can't be represented (the directory `size` field is u32), and letting
            // it through would also clamp `size` and overflow cluster sizing. A
            // 4 GiB unit-test fixture is impractical, so this mirrors the flat
            // volume's checked behavior.
            if md.len() >= u32::MAX as u64 {
                eprintln!(
                    "katea: skipping {} (>= 4 GiB, not FAT32-representable)",
                    path.display()
                );
                continue;
            }
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

/// The FAT32 data-cluster ceiling (the largest valid `count_of_clusters`); a host
/// folder demanding more than this can't be a single FAT32 volume. Per fatgen103
/// the FAT32 region tops out at `0x0FFF_FFF5` clusters; we cap at the
/// conservative `0x0FFF_FFF4` (the kernel/Windows ceiling) so the run-of-1
/// next-cluster encoding never collides with the reserved EOC/bad markers.
const FAT32_MAX_CLUSTERS: u64 = 0x0FFF_FFF4;

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
pub(crate) fn allocate(tree: &mut HostTree) -> Result<Geometry, std::io::Error> {
    // Recompute the cluster demand for whatever cluster size the loop is trying:
    // bigger clusters pack the same bytes into fewer clusters, so the demand
    // shrinks as `spc` climbs (a 500 GB folder needs ~1e9 clusters at spc=1 but
    // only ~16M at spc=64). Computed in `u64` so a huge folder can't overflow.
    let geo = fat32_geometry_for(|cluster_bytes| tree_cluster_demand(tree, cluster_bytes))?;
    // Assign clusters now that geometry is fixed.
    let cluster_bytes = u32::from(geo.spc) * SECTOR as u32;
    let mut next = ROOT_CLUSTER; // 2
    assign_dir(&mut tree.root, true, 0, &mut next, cluster_bytes);
    Ok(geo)
}

/// The largest FAT32 cluster size, in sectors. `sectors_per_cluster` tops out
/// here; once we are at this band there is no larger band to climb to, so a folder
/// that still doesn't fit is genuinely too large for one FAT32 volume.
const MAX_SPC: u8 = 64;

/// The next-larger valid `spc` band after `spc` (1 -> 8 -> 16 -> 32 -> 64),
/// mirroring `sectors_per_cluster`'s table. Used when a candidate band can't hold
/// the data: climb to the next band rather than erroring, unless already at the
/// top (`MAX_SPC`).
fn next_spc(spc: u8) -> u8 {
    match spc {
        1 => 8,
        8 => 16,
        16 => 32,
        _ => MAX_SPC, // 32 -> 64, and 64 stays 64 (the caller stops there)
    }
}

/// Pick a self-consistent, boot-valid FAT32 geometry for a tree. `demand_at`
/// returns the tree's data-cluster demand at a given cluster size in bytes; the
/// loop re-queries it each iteration because bigger clusters need fewer of them.
/// Returns `Err` only when the folder doesn't fit even at the largest cluster size
/// (`MAX_SPC` = 64, i.e. roughly > 8 TB of data) — `count_of_clusters` past
/// `FAT32_MAX_CLUSTERS` or a sector count past `u32::MAX` at a *smaller* `spc` just
/// means "climb to a bigger cluster", not "fail". All sizing is done in `u64`; the
/// final values are range-checked once before being narrowed into `u32`.
///
/// The cluster size (`spc`) the FAT and partition are sized with must match the
/// one `sectors_per_cluster` derives from the final partition size, or the BPB is
/// internally inconsistent and the disk won't boot. We reach that fixed point by
/// computing the *final* partition size each iteration (not a padded guess) and
/// re-deriving `spc` from it; if it disagrees we adopt the larger and redo. Both
/// climbs are monotone in `spc` and the per-spc partition size is ~invariant in
/// `spc`, so the loop climbs the table at most a few steps and stops.
fn fat32_geometry_for(demand_at: impl Fn(u32) -> u64) -> Result<Geometry, std::io::Error> {
    let too_large =
        || std::io::Error::other("Katea: host folder too large for a single FAT32 volume");
    let mut spc: u8 = 1;
    loop {
        // The demand for THIS cluster size: the only honest figure to size from.
        let used_data = demand_at(u32::from(spc) * SECTOR as u32);
        // Need a valid FAT32; pad with headroom (25%) so DIR shows free space and
        // writes have room, and floor at the boot-tested cluster count so the small-
        // folder partition reproduces the known-good geometry (see
        // MIN_DATA_CLUSTERS) rather than landing just under the FAT32 floor. All in
        // u64 so the +25% can't overflow before the checks below.
        let needed = used_data.max(1);
        let count_of_clusters = (needed + needed / 4).max(u64::from(MIN_DATA_CLUSTERS));
        // Too many clusters / too many data sectors for THIS band: if a bigger
        // cluster size exists, climb to it (it shrinks the cluster count); only at
        // the top band (MAX_SPC) does this mean the folder is genuinely too large.
        let data_sectors = count_of_clusters * u64::from(spc);
        if count_of_clusters > FAT32_MAX_CLUSTERS || data_sectors > u64::from(u32::MAX) {
            if spc < MAX_SPC {
                spc = next_spc(spc);
                continue;
            }
            return Err(too_large());
        }
        let data_sectors = data_sectors as u32;
        // Size the FAT from the whole partition it lives in, exactly as the flat volume does
        // (`fat_size_sectors(PART_SECTORS, spc)`): the formula's divisor accounts
        // for the FAT sectors, so passing the full partition is self-correcting.
        // We do not know `fatsz` until we size the partition, and the partition
        // size includes the FAT — so close the loop by re-deriving `fatsz` from
        // the partition built with the previous estimate until it is stable (it
        // settles in one or two steps because the data region dominates).
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
        // Validate the partition (and whole-disk) sector counts fit u32 in u64
        // before narrowing; same climb-vs-error rule as above.
        let part_sectors = u64::from(used) + u64::from(data_sectors);
        let total_sectors = u64::from(PART_START) + part_sectors;
        if total_sectors > u64::from(u32::MAX) {
            if spc < MAX_SPC {
                spc = next_spc(spc);
                continue;
            }
            return Err(too_large());
        }
        let part_sectors = part_sectors as u32;
        // Self-consistency: the spc the table picks for THIS partition must equal
        // the spc we sized with. If not, climb to it and recompute from scratch.
        let derived = sectors_per_cluster(part_sectors);
        if derived != spc {
            spc = derived;
            continue;
        }
        let geo = Geometry {
            spc,
            fatsz,
            part_start: PART_START,
            part_sectors,
            total_sectors: total_sectors as u32,
            first_data_sector: used,
            count_of_clusters: count_of_clusters as u32,
        };
        debug_assert_eq!(sectors_per_cluster(geo.part_sectors), geo.spc);
        return Ok(geo);
    }
}

/// Total data clusters the tree consumes (directories + files), for sizing.
/// Summed in `u64` so a multi-terabyte host folder can't overflow before the
/// caller (`fat32_geometry_for`) checks it against `FAT32_MAX_CLUSTERS`.
fn tree_cluster_demand(tree: &HostTree, cluster_bytes: u32) -> u64 {
    fn dir_demand(dir: &TreeDir, is_root: bool, cluster_bytes: u32) -> u64 {
        let mut n = u64::from(clusters_for(
            u64::from(entry_count(dir, is_root)) * 32,
            cluster_bytes,
        ));
        for f in &dir.files {
            n += u64::from(clusters_for(f.source.len(), cluster_bytes));
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

/// The computed-on-demand FAT. Every chain is assigned as one *contiguous
/// run* of clusters, so a used cluster's FAT entry is simply `c + 1` unless `c`
/// is the last cluster of its run (then EOC), and any cluster the tree never
/// touched is free (0). We therefore store only the set of run-end clusters and
/// `next_free` (the first never-allocated cluster) and derive every FAT entry —
/// and any FAT sector — from those, so RAM scales with the chain count rather
/// than the disk size.
#[derive(Debug)]
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

/// The FAT subdirectory attribute (ATTR_DIRECTORY); files use `ATTR_ARCHIVE`.
const ATTR_SUBDIR: u8 = 0x10;

/// Build the 32-byte directory entries for `dir` in directory order:
/// `.`/`..` first (non-root only), then files (archive attr), then
/// subdirectories (0x10). `.` points at this directory's own first cluster and
/// `..` at the parent's; a subdir entry points at the child's first cluster.
fn dir_entries(dir: &TreeDir, is_root: bool) -> Vec<[u8; 32]> {
    let mut out: Vec<[u8; 32]> = Vec::new();
    if !is_root {
        let dot = *b".          ";
        out.push(fat32_dir_entry(
            &dot,
            ATTR_SUBDIR,
            dir.first_cluster,
            0,
            0,
            0,
        ));
        // Canonical FAT (fatgen103 6.5; cf. `fat32::fat32_dot_entries`): `..` points
        // at the parent's first cluster, EXCEPT when the parent is the root, where
        // it must be 0 (the root has no real cluster number to name). The root is
        // always cluster 2 (ROOT_CLUSTER), so a parent of 2 means "root".
        let dotdot = *b"..         ";
        let dotdot_cluster = if dir.parent_first_cluster == ROOT_CLUSTER {
            0
        } else {
            dir.parent_first_cluster
        };
        out.push(fat32_dir_entry(
            &dotdot,
            ATTR_SUBDIR,
            dotdot_cluster,
            0,
            0,
            0,
        ));
    }
    for f in &dir.files {
        let size = u32::try_from(f.source.len()).unwrap_or(u32::MAX);
        out.push(fat32_dir_entry(
            &f.name,
            ATTR_ARCHIVE,
            f.first_cluster,
            0,
            0,
            size,
        ));
    }
    for s in &dir.subdirs {
        out.push(fat32_dir_entry(
            &s.name,
            ATTR_SUBDIR,
            s.dir.first_cluster,
            0,
            0,
            0,
        ));
    }
    out
}

/// One 512-byte sector (the `sector`-th, 16 entries) of `dir`'s directory data,
/// zero-padded past the last entry. `sector` indexes into the directory's entry
/// list 16 entries at a time, so the >16-entry (multi-cluster) case is served by
/// the later sectors.
///
/// Test-only: the live read path serves directory sectors via `data_sector`,
/// which inlines this slice math over the precomputed `FlatDir::entries`. The
/// module tests exercise this standalone helper directly.
#[allow(dead_code)]
pub(crate) fn dir_sector(dir: &TreeDir, is_root: bool, sector: u32) -> [u8; SECTOR] {
    let entries = dir_entries(dir, is_root);
    let mut out = [0u8; SECTOR];
    let start = (sector as usize) * 16;
    for i in 0..16usize {
        if let Some(e) = entries.get(start + i) {
            out[i * 32..i * 32 + 32].copy_from_slice(e);
        }
    }
    out
}

/// A flattened directory: just its precomputed 32-byte entries. The entries are
/// small (32 bytes each), so holding them costs RAM proportional to the entry
/// count, not the disk size. The cluster span this directory occupies lives in
/// the `runs` table, so it is not duplicated here.
#[derive(Debug)]
struct FlatDir {
    entries: Vec<[u8; 32]>,
}

/// A flattened file: its (cloned) source and byte size. The source is
/// `FileSource::HostFile { path, len }` for host files (lazy, no slurp) or
/// `InMemory` for the overlaid system files. Its cluster span lives in `runs`.
#[derive(Debug)]
struct FlatFile {
    source: FileSource,
    size: u32,
}

/// What a cluster run holds, indexing into `dirs`/`files` (no pointers).
#[derive(Debug)]
enum Role {
    Dir(usize),
    File(usize),
}

/// A lazy, read-only, whole-disk FAT32 volume over a recursive host-folder tree.
/// The sibling of the flat `KateaVolume`, generalized to a full directory tree:
/// FAT and directory sectors are computed on demand and file data is read lazily,
/// so RAM scales with the entry count rather than the disk or file sizes.
///
/// The struct is **pointer-free**: it owns only `Vec`s (the flattened dirs/files
/// and the sorted cluster-run table), the two stamped boot sectors, the geometry,
/// and the cluster index. `tree` is kept whole for tests and writes; it is
/// not consulted by `read_sector` — that path resolves a cluster through `runs`.
#[derive(Debug)]
pub(crate) struct KateaTreeVolume {
    /// The owned tree; kept for tests and writes, not read by `read_sector`.
    #[allow(dead_code)]
    tree: HostTree,
    geo: Geometry,
    /// LBA 0: the MBR with the partition entry + 0x55AA stamped in.
    mbr: [u8; SECTOR],
    /// The FAT32 VBR (at PART_START) with the BPB stamped over the boot code.
    vbr: [u8; SECTOR],
    /// FSInfo free-cluster count, served at both FSInfo sectors.
    free_count: u32,
    /// FSInfo next-free hint (= `ClusterIndex::next_free`).
    next_free: u32,
    /// The computed FAT (run-end set + next_free); generates any FAT sector.
    fat: ClusterIndex,
    /// Flattened directories, indexed by `Role::Dir`.
    dirs: Vec<FlatDir>,
    /// Flattened files, indexed by `Role::File`.
    files: Vec<FlatFile>,
    /// `(first_cluster, last_cluster, role)` runs, sorted by `first_cluster`.
    runs: Vec<(u32, u32, Role)>,
    /// Sparse write overlay: guest writes land here, reads consult it first. Held
    /// until eject with no eviction. RAM is bounded by bytes written this session.
    overlay: HashMap<u32, [u8; SECTOR]>,
    /// Directory first-cluster -> its host-filesystem path. Seeded from the tree;
    /// extended on guest MKDIR. Reconcile materializes a file in this directory to
    /// `host_path / 8.3-name`.
    dir_paths: HashMap<u32, PathBuf>,
    /// What Katea believes currently exists on the host, keyed by
    /// `(parent dir first-cluster, folded 8.3 name)`. Seeded from the tree at
    /// mount (every host file + subdir) and maintained by reconcile. Subsumes the
    /// `existing_files` (host_path for case-correct overwrite) and `materialized`
    /// (content-fingerprint dedupe). The root dir has no entry (it has no parent).
    mirrored: HashMap<(u32, [u8; 11]), MirrorEntry>,
    /// Folded 8.3 names of the InMemory boot files; never materialized to the host.
    system_names: HashSet<[u8; 11]>,
}

/// Katea's belief about one host-side entry (file or dir): where it lives, the
/// guest cluster its directory entry points at, whether it is a directory, and the
/// last content fingerprint we materialized (None until first written / for a dir).
#[derive(Clone, Debug)]
struct MirrorEntry {
    host_path: PathBuf,
    /// The guest-side first cluster this entry occupies (drives delete/rename detection).
    first_cluster: u32,
    /// True when this entry is a directory.
    is_dir: bool,
    last_fingerprint: Option<u64>,
}

/// One live directory entry seen during the gather phase.
struct LiveEntry {
    dir_cluster: u32,
    name: [u8; 11],
    first_cluster: u32,
    is_dir: bool,
}

/// One file the reconcile pass has decided to materialize: where to write it, the
/// bytes, its `(dir first-cluster, folded name)` fingerprint key, and the content
/// fingerprint recorded on a successful write. Collected during the read phase and
/// drained afterwards so no `&self` borrow is held across the host I/O.
struct PendingWrite {
    path: PathBuf,
    data: Vec<u8>,
    key: (u32, [u8; 11]),
    fingerprint: u64,
    first_cluster: u32,
}

/// A disappeared mirrored entry (file or dir) whose clusters are now claimed by
/// exactly one fresh live entry: a rename/move. The host path at `old_path` is
/// `fs::rename`d to `new_path`, and the mirror re-keyed from `old_key` to
/// `new_key`. Collected during the read phase and applied afterwards so no `&self`
/// borrow spans the I/O.
struct PendingRename {
    old_key: (u32, [u8; 11]),
    new_key: (u32, [u8; 11]),
    old_path: PathBuf,
    new_path: PathBuf,
    first_cluster: u32,
    is_dir: bool,
}

/// One disappeared entry the reconcile pass will remove from the host: the mirror
/// key, the host path, the cluster (to drop from `dir_paths` for a dir), and whether
/// it is a directory (remove_dir vs remove_file).
struct PendingDelete {
    key: (u32, [u8; 11]),
    path: PathBuf,
    first_cluster: u32,
    is_dir: bool,
}

impl KateaTreeVolume {
    /// Build the whole-disk view from the boot sectors, a host folder, and the
    /// in-memory system files overlaid at the root. Construction walks metadata
    /// only — it never reads host file *contents* (those are read lazily, one
    /// 512-byte span at a time, in `read_sector`).
    pub(crate) fn new(
        mbr: &[u8; SECTOR],
        vbr: &[u8; SECTOR],
        host_root: &Path,
        system_files: &[(String, Vec<u8>)],
    ) -> Result<Self, std::io::Error> {
        let mut tree = build_tree(host_root, system_files);
        let geo = allocate(&mut tree)?;
        let fat = ClusterIndex::build(&tree, &geo);
        let next_free = fat.next_free();
        // Used data clusters are 2..next_free; the rest of the addressable range
        // is free. `saturating_sub` guards the (impossible) empty-disk underflow.
        let free_count = geo
            .count_of_clusters
            .saturating_sub(next_free - ROOT_CLUSTER);

        // --- MBR: stamp the single partition entry + signature, with the dynamic
        // partition size (mirrors KateaVolume::new but `geo`-driven). ----------
        let mut mbr_out = *mbr;
        let pe = 0x1BE; // first partition entry
        mbr_out[pe] = 0x80; // active / bootable
        mbr_out[pe + 1..pe + 4].copy_from_slice(&lba_to_chs(geo.part_start));
        mbr_out[pe + 4] = PART_TYPE_FAT32_LBA;
        mbr_out[pe + 5..pe + 8].copy_from_slice(&lba_to_chs(geo.part_start + geo.part_sectors - 1));
        mbr_out[pe + 8..pe + 12].copy_from_slice(&geo.part_start.to_le_bytes()); // RelSect
        mbr_out[pe + 12..pe + 16].copy_from_slice(&geo.part_sectors.to_le_bytes()); // NumSect
        mbr_out[0x1FE] = 0x55;
        mbr_out[0x1FF] = 0xAA;

        // --- VBR: stamp the FAT32 BPB over the boot code, keeping the boot code.
        let mut vbr_out = *vbr;
        stamp_fat32_bpb(
            &mut vbr_out,
            geo.spc,
            geo.fatsz,
            geo.part_start,
            geo.part_sectors,
        );

        // --- seed the write maps from the (allocated) tree ----------------------
        let mut dir_paths: HashMap<u32, PathBuf> = HashMap::new();
        let mut mirrored: HashMap<(u32, [u8; 11]), MirrorEntry> = HashMap::new();
        fn seed(
            dir: &TreeDir,
            dir_paths: &mut HashMap<u32, PathBuf>,
            mirrored: &mut HashMap<(u32, [u8; 11]), MirrorEntry>,
        ) {
            dir_paths.insert(dir.first_cluster, dir.host_path.clone());
            for f in &dir.files {
                if let FileSource::HostFile { path, .. } = &f.source {
                    mirrored.insert(
                        (dir.first_cluster, f.name),
                        MirrorEntry {
                            host_path: path.clone(),
                            first_cluster: f.first_cluster,
                            is_dir: false,
                            last_fingerprint: None,
                        },
                    );
                }
            }
            for s in &dir.subdirs {
                mirrored.insert(
                    (dir.first_cluster, s.name),
                    MirrorEntry {
                        host_path: s.dir.host_path.clone(),
                        first_cluster: s.dir.first_cluster,
                        is_dir: true,
                        last_fingerprint: None,
                    },
                );
                seed(&s.dir, dir_paths, mirrored);
            }
        }
        seed(&tree.root, &mut dir_paths, &mut mirrored);
        let mut system_names: HashSet<[u8; 11]> = system_files
            .iter()
            .map(|(name, _)| fold_literal_83(name))
            .collect();
        // The synthetic C:\DOS folder build_tree creates for the system binaries must
        // also never be materialized as a real host directory: protect its name so
        // reconcile's classify() skips the DOS subdir entry (exactly the condition
        // under which build_tree attaches the folder).
        if system_files.iter().any(|(name, _)| {
            DOS_FOLDER_BINARIES
                .iter()
                .any(|b| name.eq_ignore_ascii_case(b))
        }) {
            system_names.insert(fold_literal_83("DOS"));
        }

        // --- flatten the tree into dirs/files + the sorted run table -----------
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        let mut runs = Vec::new();
        flatten(&tree.root, true, &mut dirs, &mut files, &mut runs);
        runs.sort_by_key(|r| r.0);

        Ok(Self {
            tree,
            geo,
            mbr: mbr_out,
            vbr: vbr_out,
            free_count,
            next_free,
            fat,
            dirs,
            files,
            runs,
            overlay: HashMap::new(),
            dir_paths,
            mirrored,
            system_names,
        })
    }

    /// The whole-disk sector count, so the ATA layer can derive its geometry.
    pub(crate) fn total_sectors(&self) -> u32 {
        self.geo.total_sectors
    }

    /// The owned tree for tests and writes; `read_sector` does not use it.
    #[allow(dead_code)]
    pub(crate) fn tree(&self) -> &HostTree {
        &self.tree
    }

    /// The 28-bit FAT entry for cluster `c`, reading the overlay-shadowed FAT (so
    /// guest-written chain links are honored) and falling back to the tree's
    /// computed FAT. Built on `read_sector`, which already consults the overlay.
    pub(crate) fn fat_entry(&self, c: u32) -> u32 {
        let byte = c as usize * 4;
        let fat_sector_rel = u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let lba = self.geo.part_start + fat_sector_rel;
        let off = byte % SECTOR;
        let sec = self.read_sector(lba);
        u32::from_le_bytes([sec[off], sec[off + 1], sec[off + 2], sec[off + 3]]) & 0x0FFF_FFFF
    }

    /// Absolute LBA of a data cluster's first sector. The reconcile pass and the
    /// tests resolve cluster -> LBA this way; the read path goes the other way
    /// (LBA -> cluster) inside `read_sector`.
    pub(crate) fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.geo.part_start
            + self.geo.first_data_sector
            + (cluster - ROOT_CLUSTER) * u32::from(self.geo.spc)
    }

    /// The maximum cluster-chain length we will follow before treating a chain as
    /// corrupt (a cyclic/garbled FAT must not loop forever). The data region can
    /// never exceed the cluster count, so this is a safe ceiling.
    fn max_chain(&self) -> usize {
        self.geo.count_of_clusters as usize + 2
    }

    /// Whether `c` is a valid data cluster (`2 ..= count_of_clusters + 1`). A FAT
    /// link outside this range is a corrupt or guest-crafted pointer; reconcile
    /// holds such a chain rather than computing an out-of-range LBA (which would
    /// overflow `u32` in debug for large cluster sizes — and reads garbage in
    /// release). Conservative: never materialize from an out-of-range chain.
    fn cluster_in_range(&self, c: u32) -> bool {
        (ROOT_CLUSTER..=self.geo.count_of_clusters + 1).contains(&c)
    }

    /// Read-only walk of every known directory, collecting its live entries (files
    /// and subdirs, skipping dots/LFN/volume-label/system names). The basis for
    /// detecting disappearances (delete/rename) in `reconcile`.
    fn gather_live(&self) -> Vec<LiveEntry> {
        let spc = u32::from(self.geo.spc);
        let cluster_bytes = spc as usize * SECTOR;
        let max = self.max_chain();
        let mut work: Vec<u32> = self.dir_paths.keys().copied().collect();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut out = Vec::new();
        while let Some(dir_cluster) = work.pop() {
            if !seen.insert(dir_cluster) {
                continue;
            }
            let Some(dir_chain) =
                crate::katea_write::chain(dir_cluster, max, |c| self.fat_entry(c))
            else {
                continue;
            };
            if dir_chain.iter().any(|&c| !self.cluster_in_range(c)) {
                continue;
            }
            let mut dir_bytes = Vec::with_capacity(dir_chain.len() * cluster_bytes);
            for c in &dir_chain {
                let base = self.cluster_to_lba(*c);
                for s in 0..spc {
                    dir_bytes.extend_from_slice(&self.read_sector(base + s));
                }
            }
            for e in crate::katea_write::parse_dir(&dir_bytes) {
                match crate::katea_write::classify(&e, &self.system_names) {
                    crate::katea_write::EntryAction::Skip => {}
                    crate::katea_write::EntryAction::MakeDir {
                        name,
                        first_cluster,
                    } => {
                        out.push(LiveEntry {
                            dir_cluster,
                            name,
                            first_cluster,
                            is_dir: true,
                        });
                        // Only descend into dirs Katea already knows; a just-MKDIR'd
                        // subdir is registered by the materialize phase first, then
                        // gathered on the next pass.
                        if self.dir_paths.contains_key(&first_cluster) {
                            work.push(first_cluster);
                        }
                    }
                    crate::katea_write::EntryAction::MakeFile {
                        name,
                        first_cluster,
                        ..
                    } => {
                        out.push(LiveEntry {
                            dir_cluster,
                            name,
                            first_cluster,
                            is_dir: false,
                        });
                    }
                }
            }
        }
        out
    }

    /// Reconcile the overlay to the host folder: walk every known directory and
    /// atomically materialize each *complete, touched, changed* 8.3 file, and
    /// create host subdirectories for MKDIR'd entries. Conservative — an
    /// incomplete or ambiguous entry is held in the overlay and retried next pass.
    /// Called after each HostFolder ATA write command and at eject/flush.
    pub(crate) fn reconcile(&mut self) {
        // PHASE 1: gather all live entries (read-only).
        let live = self.gather_live();
        let live_keys: HashSet<(u32, [u8; 11])> =
            live.iter().map(|l| (l.dir_cluster, l.name)).collect();
        // first_cluster -> the live entries claiming it (for rename matching in phase 2).
        let mut live_by_cluster: HashMap<u32, Vec<(u32, [u8; 11], bool)>> = HashMap::new();
        for l in &live {
            live_by_cluster.entry(l.first_cluster).or_default().push((
                l.dir_cluster,
                l.name,
                l.is_dir,
            ));
        }

        // PHASE 2: disappearances — a mirrored entry no longer live is a rename/move
        // (a fresh entry claims its clusters), a delete (chain freed, no claimant), or
        // held. Collected read-only, then applied. `handled` tells phase 3 to skip an
        // entry already moved/renamed here.
        let mut deletes: Vec<PendingDelete> = Vec::new();
        let mut renames: Vec<PendingRename> = Vec::new();
        let mut handled: HashSet<(u32, [u8; 11])> = HashSet::new();
        for (key, m) in &self.mirrored {
            if live_keys.contains(key) {
                continue; // still live
            }
            // Rename/move: exactly one FRESH live entry (not already mirrored) holds
            // this entry's clusters, with a real cluster and matching kind.
            if m.first_cluster >= 2 {
                if let Some(claimants) = live_by_cluster.get(&m.first_cluster) {
                    let fresh: Vec<&(u32, [u8; 11], bool)> = claimants
                        .iter()
                        .filter(|(d, n, is_dir)| {
                            *is_dir == m.is_dir
                                && !self.mirrored.contains_key(&(*d, *n))
                                // Already taken as another disappearance's rename target
                                // this pass (only reachable under guest-aliased clusters);
                                // hold rather than clobber the first rename's destination.
                                && !handled.contains(&(*d, *n))
                        })
                        .collect();
                    if fresh.len() == 1 {
                        let &(ndir, nname, _) = fresh[0];
                        if let Some(host_dir) = self.dir_paths.get(&ndir).cloned() {
                            let new_path = host_dir.join(crate::katea_volume::decode_83(&nname));
                            renames.push(PendingRename {
                                old_key: *key,
                                new_key: (ndir, nname),
                                old_path: m.host_path.clone(),
                                new_path,
                                first_cluster: m.first_cluster,
                                is_dir: m.is_dir,
                            });
                            handled.insert((ndir, nname));
                            continue;
                        }
                    }
                }
            }
            // Delete: empty file/dir, or chain freed with no claimant. Else HOLD.
            let freed = m.first_cluster < 2 || self.fat_entry(m.first_cluster) == 0;
            let claimed = live_by_cluster.contains_key(&m.first_cluster);
            if freed && !claimed {
                deletes.push(PendingDelete {
                    key: *key,
                    path: m.host_path.clone(),
                    first_cluster: m.first_cluster,
                    is_dir: m.is_dir,
                });
            }
        }
        // Apply renames first (so a move's source path still exists), then deletes.
        for r in renames {
            if let Err(e) = std::fs::rename(&r.old_path, &r.new_path) {
                eprintln!(
                    "katea: rename {} -> {} failed: {e}",
                    r.old_path.display(),
                    r.new_path.display()
                );
                continue;
            }
            self.mirrored.remove(&r.old_key);
            self.mirrored.insert(
                r.new_key,
                MirrorEntry {
                    host_path: r.new_path.clone(),
                    first_cluster: r.first_cluster,
                    is_dir: r.is_dir,
                    last_fingerprint: None,
                },
            );
            if r.is_dir {
                self.dir_paths.insert(r.first_cluster, r.new_path);
            }
        }
        // Files before dirs, so a just-emptied dir's host files are gone before we
        // try to remove the (now-empty) host dir. (bool Ord: false(file) < true(dir).)
        deletes.sort_by_key(|d| d.is_dir);
        for d in deletes {
            let res = if d.is_dir {
                std::fs::remove_dir(&d.path) // fails (held) if the host dir is non-empty
            } else {
                std::fs::remove_file(&d.path)
            };
            if let Err(e) = res {
                if d.path.exists() {
                    eprintln!("katea: delete {} failed: {e}", d.path.display());
                    continue; // hold
                }
            }
            self.mirrored.remove(&d.key);
            if d.is_dir {
                self.dir_paths.remove(&d.first_cluster);
            }
        }

        let spc = u32::from(self.geo.spc);
        let cluster_bytes = spc as usize * SECTOR;
        let max = self.max_chain();

        // PHASE 3: materialize creates/overwrites/grows + mkdir.
        // Fixpoint over known directories: MKDIR registers a new directory which is
        // pushed onto the worklist so its files are materialized in the same pass.
        let mut work: Vec<u32> = self.dir_paths.keys().copied().collect();
        let mut seen: HashSet<u32> = HashSet::new();

        while let Some(dir_cluster) = work.pop() {
            if !seen.insert(dir_cluster) {
                continue;
            }
            let Some(host_dir) = self.dir_paths.get(&dir_cluster).cloned() else {
                continue;
            };

            // Read the directory's full bytes by following its own cluster chain.
            let Some(dir_chain) =
                crate::katea_write::chain(dir_cluster, max, |c| self.fat_entry(c))
            else {
                continue; // a corrupt directory chain: hold the whole directory
            };
            if dir_chain.iter().any(|&c| !self.cluster_in_range(c)) {
                continue; // a chain link outside the data region: hold this dir
            }
            let mut dir_bytes = Vec::with_capacity(dir_chain.len() * cluster_bytes);
            for c in &dir_chain {
                let base = self.cluster_to_lba(*c);
                for s in 0..spc {
                    dir_bytes.extend_from_slice(&self.read_sector(base + s));
                }
            }
            let dir_written = dir_chain.iter().any(|c| {
                let base = self.cluster_to_lba(*c);
                (0..spc).any(|s| self.overlay.contains_key(&(base + s)))
            });

            // Decide every entry (read-only); collect actions, then apply.
            let mut mkdirs: Vec<(u32, [u8; 11], u32, std::path::PathBuf)> = Vec::new();
            let mut writes: Vec<PendingWrite> = Vec::new();

            for e in crate::katea_write::parse_dir(&dir_bytes) {
                match crate::katea_write::classify(&e, &self.system_names) {
                    crate::katea_write::EntryAction::Skip => {}
                    crate::katea_write::EntryAction::MakeDir {
                        name,
                        first_cluster,
                    } => {
                        if handled.contains(&(dir_cluster, name)) {
                            continue; // already renamed in phase 2 (symmetry with MakeFile)
                        }
                        let path = host_dir.join(crate::katea_volume::decode_83(&name));
                        mkdirs.push((dir_cluster, name, first_cluster, path));
                    }
                    crate::katea_write::EntryAction::MakeFile {
                        name,
                        first_cluster,
                        size,
                    } => {
                        let Some(fchain) =
                            crate::katea_write::chain(first_cluster, max, |c| self.fat_entry(c))
                        else {
                            continue; // incomplete/corrupt chain: hold
                        };
                        if fchain.iter().any(|&c| !self.cluster_in_range(c)) {
                            continue; // chain references an out-of-range cluster: hold
                        }
                        let capacity = fchain.len() as u64 * cluster_bytes as u64;
                        if u64::from(size) > capacity {
                            continue; // not enough clusters yet: hold
                        }
                        // Touched this session? Non-empty files must have overlay
                        // data; a brand-new name in a written directory counts even
                        // when empty. Untouched tree/system files are skipped.
                        let data_written = fchain.iter().any(|c| {
                            let base = self.cluster_to_lba(*c);
                            (0..spc).any(|s| self.overlay.contains_key(&(base + s)))
                        });
                        let is_new = !self.mirrored.contains_key(&(dir_cluster, name));
                        if handled.contains(&(dir_cluster, name)) {
                            continue; // already moved/renamed in phase 2
                        }
                        if !(data_written || (dir_written && is_new)) {
                            continue;
                        }

                        // Gather the file bytes (overlay-then-tree), truncate to size.
                        let mut data = Vec::with_capacity(size as usize);
                        'gather: for c in &fchain {
                            let base = self.cluster_to_lba(*c);
                            for s in 0..spc {
                                if data.len() >= size as usize {
                                    break 'gather;
                                }
                                data.extend_from_slice(&self.read_sector(base + s));
                            }
                        }
                        data.truncate(size as usize);

                        let fp = crate::katea_write::fingerprint(&data);
                        if self
                            .mirrored
                            .get(&(dir_cluster, name))
                            .and_then(|m| m.last_fingerprint)
                            == Some(fp)
                        {
                            continue; // unchanged since last pass
                        }
                        let host_path = self
                            .mirrored
                            .get(&(dir_cluster, name))
                            .map(|m| m.host_path.clone())
                            .unwrap_or_else(|| {
                                host_dir.join(crate::katea_volume::decode_83(&name))
                            });
                        writes.push(PendingWrite {
                            path: host_path,
                            data,
                            key: (dir_cluster, name),
                            fingerprint: fp,
                            first_cluster,
                        });
                    }
                }
            }

            // Apply mutations + host I/O (no `self` borrow held across reads now).
            for (parent, name, first_cluster, path) in mkdirs {
                if let Err(e) = std::fs::create_dir_all(&path) {
                    eprintln!("katea: mkdir {} failed: {e}", path.display());
                    continue;
                }
                self.dir_paths.entry(first_cluster).or_insert(path.clone());
                self.mirrored.entry((parent, name)).or_insert(MirrorEntry {
                    host_path: path,
                    first_cluster,
                    is_dir: true,
                    last_fingerprint: None,
                });
                if !seen.contains(&first_cluster) {
                    work.push(first_cluster);
                }
            }
            for w in writes {
                match crate::katea_write::atomic_write(&w.path, &w.data) {
                    Ok(()) => {
                        self.mirrored.insert(
                            w.key,
                            MirrorEntry {
                                host_path: w.path.clone(),
                                first_cluster: w.first_cluster,
                                is_dir: false,
                                last_fingerprint: Some(w.fingerprint),
                            },
                        );
                    }
                    Err(e) => {
                        // Hold on failure: the real host file is untouched; retry
                        // next pass. atomic_write guarantees no torn file.
                        eprintln!("katea: materialize {} failed: {e}", w.path.display());
                    }
                }
            }
        }
    }

    /// Store one guest-written sector in the overlay. Reads of this LBA now return
    /// `data` until eject. The interpreter (`reconcile`) reads the overlay to
    /// mirror finished files to the host folder.
    pub(crate) fn write_sector(&mut self, lba: u32, data: &[u8; SECTOR]) {
        self.overlay.insert(lba, *data);
    }

    /// Read one whole-disk sector by absolute LBA. Resolves entirely from
    /// in-memory metadata except for `HostFile` data, read on demand. Out-of-range
    /// or unmapped sectors read back as zeros.
    pub(crate) fn read_sector(&self, lba: u32) -> [u8; SECTOR] {
        if let Some(s) = self.overlay.get(&lba) {
            return *s;
        }
        if lba == 0 {
            return self.mbr;
        }
        if lba < self.geo.part_start {
            return [0u8; SECTOR];
        }
        let rel = lba - self.geo.part_start; // partition-relative sector

        // Reserved area: VBR (0), FSInfo (1), backup boot (6), backup FSInfo (7).
        if rel == 0 || rel == u32::from(BACKUP_BOOT_SECTOR) {
            return self.vbr;
        }
        if rel == u32::from(FSINFO_SECTOR) || rel == u32::from(BACKUP_FSINFO_SECTOR) {
            return fat32_fsinfo_sector(self.free_count, self.next_free);
        }

        // FAT region: NUM_FATS identical copies, each `fatsz` long.
        let reserved = u32::from(RESERVED_SECTORS);
        let fat_end = reserved + u32::from(NUM_FATS) * self.geo.fatsz;
        if (reserved..fat_end).contains(&rel) {
            let within = (rel - reserved) % self.geo.fatsz;
            return self.fat.fat_sector(within, &self.geo);
        }

        // Data region: cluster 2 begins at `first_data_sector`.
        if rel >= self.geo.first_data_sector {
            let data_lba = rel - self.geo.first_data_sector;
            let spc = u32::from(self.geo.spc);
            let cluster = ROOT_CLUSTER + data_lba / spc;
            let sector_in_cluster = data_lba % spc;
            return self.data_sector(cluster, sector_in_cluster);
        }

        [0u8; SECTOR]
    }

    /// Resolve one data-region sector by finding the run owning `cluster`, then
    /// serving directory entries or lazy file bytes. A cluster in no run is free
    /// space (zeros).
    fn data_sector(&self, cluster: u32, sector_in_cluster: u32) -> [u8; SECTOR] {
        let Some(run) = self
            .runs
            .iter()
            .find(|(first, last, _)| cluster >= *first && cluster <= *last)
        else {
            return [0u8; SECTOR]; // free space
        };
        let cluster_off = cluster - run.0; // cluster index within the run
        let spc = u32::from(self.geo.spc);
        match &run.2 {
            Role::Dir(id) => {
                let d = &self.dirs[*id];
                let sector_in_dir = cluster_off * spc + sector_in_cluster;
                let mut out = [0u8; SECTOR];
                let start = (sector_in_dir as usize) * 16;
                for i in 0..16usize {
                    if let Some(e) = d.entries.get(start + i) {
                        out[i * 32..i * 32 + 32].copy_from_slice(e);
                    }
                }
                out
            }
            Role::File(id) => {
                let f = &self.files[*id];
                let byte_off = u64::from(cluster_off) * u64::from(spc) * SECTOR as u64
                    + u64::from(sector_in_cluster) * SECTOR as u64;
                read_source_span(&f.source, byte_off, f.size)
            }
        }
    }
}

/// Flatten the tree depth-first into `dirs`/`files` + the cluster-run table. The
/// recursion order matches `assign_dir` (dir chain, then its files, then its
/// subdirs), but `read_sector` searches `runs` by cluster, so the order is not
/// load-bearing for reads — only for keeping each entity's run contiguous.
fn flatten(
    dir: &TreeDir,
    is_root: bool,
    dirs: &mut Vec<FlatDir>,
    files: &mut Vec<FlatFile>,
    runs: &mut Vec<(u32, u32, Role)>,
) {
    let id = dirs.len();
    dirs.push(FlatDir {
        entries: dir_entries(dir, is_root),
    });
    runs.push((
        dir.first_cluster,
        dir.first_cluster + dir.cluster_count - 1,
        Role::Dir(id),
    ));
    for f in &dir.files {
        let fid = files.len();
        files.push(FlatFile {
            source: clone_source(&f.source),
            size: u32::try_from(f.source.len()).unwrap_or(u32::MAX),
        });
        runs.push((
            f.first_cluster,
            f.first_cluster + f.cluster_count - 1,
            Role::File(fid),
        ));
    }
    for s in &dir.subdirs {
        flatten(&s.dir, false, dirs, files, runs);
    }
}

/// `FileSource` is not `Clone` (it holds a `Vec`); clone it explicitly.
fn clone_source(s: &FileSource) -> FileSource {
    match s {
        FileSource::InMemory(v) => FileSource::InMemory(v.clone()),
        FileSource::HostFile { path, len } => FileSource::HostFile {
            path: path.clone(),
            len: *len,
        },
    }
}

/// Read one 512-byte span at `byte_off` from a source, zero-padding past `size`.
/// Same contract as `katea_volume::read_source_span`: a `HostFile` opens,
/// seeks, and reads exactly the in-file portion on demand; an I/O error logs and
/// reads back as zeros so a vanished/shrunk host file can't panic the guest.
fn read_source_span(source: &FileSource, byte_off: u64, size: u32) -> [u8; SECTOR] {
    let mut out = [0u8; SECTOR];
    let valid = u64::from(size).saturating_sub(byte_off).min(SECTOR as u64) as usize;
    if valid == 0 {
        return out;
    }
    match source {
        FileSource::InMemory(v) => {
            let start = byte_off as usize;
            // `valid` derives from the declared `size`; clamp it to what the backing
            // Vec actually holds so a size that disagrees with `v.len()` can never
            // panic the read. The padded tail stays zero.
            let avail = valid.min(v.len().saturating_sub(start));
            out[..avail].copy_from_slice(&v[start..start + avail]);
        }
        FileSource::HostFile { path, .. } => {
            match File::open(path).and_then(|mut f| {
                f.seek(SeekFrom::Start(byte_off))?;
                f.read_exact(&mut out[..valid])
            }) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("katea: read {} @ {byte_off}: {e}", path.display());
                    out = [0u8; SECTOR];
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "katea_tree_test.rs"]
mod tests;
