//! Recursive, read-only, lazy host-folder directory tree for the Katea
//! controller. Generalizes the flat `KateaVolume` (M0) into a full FAT32
//! directory tree whose FAT and directory sectors are computed on demand, so
//! RAM scales with the entry count rather than the disk or file sizes.

// `KateaTreeVolume` is now consumed by the ATA `HostFolder` backing (`ata.rs`)
// and `mount_hdd_folder` (`lib.rs`), so the read path (`new`/`read_sector`/
// `total_sectors`) is live and the module-level dead-code allow is gone. A few
// items remain reachable only from this module's `#[cfg(test)]` tests; each
// carries a narrow `#[allow(dead_code)]` at its definition: `tree()`/
// `cluster_to_lba()` and the `tree` field they read, and the free `dir_sector`
// (the per-volume read path inlines the same logic in `data_sector`). The `tree`
// field is also the seam the M2 write engine will read.

use crate::fat32::{FAT32_EOC, fat32_dir_entry, fat32_fsinfo_sector};
use crate::katea_names::NameTable;
use crate::katea_volume::{
    ATTR_ARCHIVE, BACKUP_BOOT_SECTOR, BACKUP_FSINFO_SECTOR, FAT0_MEDIA, FSINFO_SECTOR, FileSource,
    NUM_FATS, PART_START, PART_TYPE_FAT32_LBA, RESERVED_SECTORS, ROOT_CLUSTER, SECTOR,
    fat_size_sectors, lba_to_chs, sectors_per_cluster, stamp_fat32_bpb,
};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
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

// `walk_into` is both the root entry (names already holds the system
// reservations) and the per-subdirectory recursion; each subdirectory gets its
// own fresh `NameTable` at the call site.
fn walk_into(host: &Path, dir: &mut TreeDir, names: &mut NameTable, depth: usize) {
    if depth > MAX_DEPTH {
        // A too-deep folder is truncated rather than recursed forever; warn once
        // at the cap so the loss isn't silent (L2).
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
            continue; // ponytail: no symlink following in M1 (loop-safe)
        }
        let path = e.path();
        if ft.is_dir() {
            let name = names.add_host(&path, true);
            let mut child = TreeDir::default();
            let mut child_names = NameTable::new(); // a fresh table per directory
            walk_into(&path, &mut child, &mut child_names, depth + 1);
            dir.subdirs.push(TreeSubdir { name, dir: child });
        } else if ft.is_file() {
            let Ok(md) = e.metadata() else { continue };
            // Skip any file too large for a FAT32 32-bit size/cluster span, exactly
            // as M0's `KateaVolume::new` does (`katea_volume.rs`): a `>= 4 GiB` file
            // can't be represented (the directory `size` field is u32), and letting
            // it through would also clamp `size` wrong and feed the C1 cluster
            // overflow. No unit test — a 4 GiB fixture is infeasible — so this
            // mirrors M0's untested-but-correct pattern (M1).
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
/// final values are range-checked once before being narrowed into `u32`. (C1)
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
        // M2 has room, and floor at the boot-tested M0 cluster count so the small-
        // folder partition reproduces M0's known-good geometry (see
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
        // Size the FAT from the whole partition it lives in, exactly as M0 does
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

/// The computed-on-demand FAT. Task 4 assigns every chain as one *contiguous
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
        // always cluster 2 (ROOT_CLUSTER), so a parent of 2 means "root". (M2)
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
/// The sibling of M0's flat `KateaVolume`, generalized to a full directory tree:
/// FAT and directory sectors are computed on demand and file data is read lazily,
/// so RAM scales with the entry count rather than the disk or file sizes.
///
/// The struct is **pointer-free**: it owns only `Vec`s (the flattened dirs/files
/// and the sorted cluster-run table), the two stamped boot sectors, the geometry,
/// and the cluster index. `tree` is kept whole for the test and M2 (writes); it is
/// not consulted by `read_sector` — that path resolves a cluster through `runs`.
#[derive(Debug)]
pub(crate) struct KateaTreeVolume {
    /// The owned tree; kept for tests / M2 (writes), not read by `read_sector`.
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
        })
    }

    /// The whole-disk sector count, so the ATA layer can derive its geometry.
    pub(crate) fn total_sectors(&self) -> u32 {
        self.geo.total_sectors
    }

    /// The owned tree (for tests / M2 writes; `read_sector` does not use it).
    #[allow(dead_code)]
    pub(crate) fn tree(&self) -> &HostTree {
        &self.tree
    }

    /// Absolute LBA of a data cluster's first sector. Test-only: the read path
    /// goes the other way (LBA -> cluster) inside `read_sector`.
    #[allow(dead_code)]
    pub(crate) fn cluster_to_lba(&self, cluster: u32) -> u32 {
        self.geo.part_start
            + self.geo.first_data_sector
            + (cluster - ROOT_CLUSTER) * u32::from(self.geo.spc)
    }

    /// Read one whole-disk sector by absolute LBA. Resolves entirely from
    /// in-memory metadata except for `HostFile` data, read on demand. Out-of-range
    /// or unmapped sectors read back as zeros.
    pub(crate) fn read_sector(&self, lba: u32) -> [u8; SECTOR] {
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
/// Same contract as M0's `katea_volume::read_source_span`: a `HostFile` opens,
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
            // panic the slice (L1). The padded tail stays zero.
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
        let geo = allocate(&mut tree).expect("small folder fits a FAT32 volume");

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
        let geo = allocate(&mut tree).expect("small folder fits a FAT32 volume");
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

    #[test]
    fn directory_sector_emits_dot_dotdot_files_and_subdir_entries() {
        let root = scratch("dir");
        std::fs::create_dir_all(root.join("SUB")).unwrap();
        std::fs::write(root.join("SUB/A.TXT"), b"hi").unwrap();
        let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 10])];
        let mut tree = build_tree(&root, &sys);
        allocate(&mut tree).expect("small folder fits a FAT32 volume");

        // Root sector 0: entry 0 = KERNEL.SYS (archive), and a SUB subdir entry (0x10).
        let rootsec = dir_sector(&tree.root, true, 0);
        assert_eq!(&rootsec[0..11], b"KERNEL  SYS");
        assert_eq!(rootsec[11], 0x20); // archive
        let sub = &tree.root.subdirs[0];
        // Find SUB's 32-byte entry in the root sector.
        let pos = (0..16)
            .map(|i| i * 32)
            .find(|&o| &rootsec[o..o + 11] == b"SUB        ")
            .unwrap();
        assert_eq!(rootsec[pos + 11] & 0x10, 0x10, "subdir attribute");

        // SUB sector 0: `.` then `..`, then A.TXT.
        let subsec = dir_sector(&sub.dir, false, 0);
        assert_eq!(&subsec[0..11], b".          ");
        assert_eq!(subsec[11] & 0x10, 0x10);
        assert_eq!(&subsec[32..43], b"..         ");
        assert_eq!(&subsec[64..75], b"A       TXT");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn directory_spanning_multiple_clusters_serves_later_sectors() {
        let root = scratch("multiclu");
        for i in 0..20 {
            std::fs::write(root.join(format!("F{i:02}.TXT")), b"x").unwrap();
        }
        let mut tree = build_tree(&root, &[]);
        allocate(&mut tree).expect("small folder fits a FAT32 volume");
        // 20 file entries (16 per 512B sector at spc=1) need more than one cluster.
        assert!(
            tree.root.cluster_count >= 2,
            "20 entries need > 1 cluster at spc=1"
        );
        // Second sector (entries 16..32 in directory order) holds the 17th+ entries.
        let s1 = dir_sector(&tree.root, true, 1);
        // The walk sorts F00.TXT..F19.TXT and there are no subdirs/system files,
        // so the 17th directory entry (0-based index 16) is F16.TXT.
        assert_eq!(
            &s1[0..11],
            b"F16     TXT",
            "sector 1, entry 0 is the 17th file"
        );
        assert_eq!(s1[11], crate::katea_volume::ATTR_ARCHIVE, "a file entry");
        // The 20th (last) file lands at index 19 -> sector 1, entry 3.
        assert_eq!(
            &s1[3 * 32..3 * 32 + 11],
            b"F19     TXT",
            "entry 19 is F19.TXT"
        );
        // Entries past the 20th are zero-padded.
        assert_eq!(s1[4 * 32], 0x00, "no entry past the last file");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_sector_serves_mbr_vbr_dirs_and_lazy_file_data_at_depth() {
        let root = scratch("vol");
        std::fs::create_dir_all(root.join("GAMES/HELLO")).unwrap();
        std::fs::write(
            root.join("GAMES/HELLO/HELLO.COM"),
            (0..600u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>(),
        )
        .unwrap();
        let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];

        // Borrow the real boot sectors from the committed image (any 512-byte
        // MBR/VBR with a 55AA signature works for the unit test).
        let img = izarravm_firmware::tokados_hdd_img();
        let mut mbr = [0u8; 512];
        mbr.copy_from_slice(&img[0..512]);
        let mut vbr = [0u8; 512];
        vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);

        let vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys)
            .expect("small folder fits a FAT32 volume");

        // LBA 0 = MBR with the partition entry + 55AA.
        let s0 = vol.read_sector(0);
        assert_eq!(s0[0x1FE], 0x55);
        assert_eq!(s0[0x1FF], 0xAA);
        // VBR at PART_START has the FAT32 BPB signature.
        let vbr_lba = 2048;
        let sv = vol.read_sector(vbr_lba);
        assert_eq!(sv[0x1FE], 0x55);
        assert_eq!(sv[0x1FF], 0xAA);

        // Walk to HELLO.COM's first data sector and verify lazy bytes match host.
        let games = &vol.tree().root.subdirs[0].dir;
        let hello = &games.subdirs[0].dir;
        let f = &hello.files[0];
        let lba = vol.cluster_to_lba(f.first_cluster);
        let data = vol.read_sector(lba);
        let expect: Vec<u8> = (0..512u32).map(|i| (i % 251) as u8).collect();
        assert_eq!(&data[..], &expect[..]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn geometry_bounds_a_huge_folder_and_reproduces_m0_for_a_small_one() {
        // The demand callback returns the cluster count for a given cluster size in
        // bytes; the geometry loop re-queries it as `spc` climbs, because bigger
        // clusters hold the same bytes in fewer of them (see `fat32_geometry_for`).

        // C1: a folder whose demand stays enormous at EVERY cluster size (here a
        // flat ~80 billion clusters, i.e. roughly > 8 TB of data) doesn't fit even
        // at the largest cluster size (spc=64) -> must fail loudly, not overflow.
        let huge = fat32_geometry_for(|_cb| 80_000_000_000);
        assert!(
            huge.is_err(),
            "an ~80-billion-cluster demand exceeds FAT32 at every cluster size"
        );

        // Regression guard (the bug the per-spc recompute fixes): a ~500 GB folder
        // demands ~1e9 clusters at spc=1 (over the FAT32 ceiling) but only ~16M at
        // spc=64, so it MUST be accepted on the large-cluster band rather than
        // wrongly rejected as "too large".
        let geo = fat32_geometry_for(|cb| (500u64 << 30) / u64::from(cb))
            .expect("a 500 GB folder fits FAT32 at a large cluster size");
        assert_eq!(geo.spc, 64, "500 GB lands on the largest cluster band");
        assert!(
            u64::from(geo.count_of_clusters) < FAT32_MAX_CLUSTERS,
            "count_of_clusters stays under the FAT32 ceiling at spc=64"
        );
        // `count * spc` (the data sectors) must not have overflowed u32.
        assert!(u64::from(geo.count_of_clusters) * u64::from(geo.spc) <= u64::from(u32::MAX));
        assert_eq!(sectors_per_cluster(geo.part_sectors), geo.spc);

        // A ~1 GB folder must be sized to ~the data (a few GB of sectors), NOT the
        // ~80 GB the old spc=1-fixed demand would have produced at spc=64. Proves
        // the demand was recomputed for the chosen cluster size.
        let geo1g = fat32_geometry_for(|cb| (1u64 << 30) / u64::from(cb))
            .expect("a 1 GB folder fits FAT32");
        let two_gib_sectors = (2u64 << 30) / SECTOR as u64; // ~4.2M sectors
        assert!(
            u64::from(geo1g.total_sectors) < 8 * two_gib_sectors,
            "1 GB of files must not balloon to an ~80 GB disk (got {} sectors)",
            geo1g.total_sectors
        );
        assert_eq!(sectors_per_cluster(geo1g.part_sectors), geo1g.spc);

        // A tiny demand floors at MIN_DATA_CLUSTERS and reproduces M0's exact,
        // boot-tested geometry: spc=1, fatsz=741, count_of_clusters=94742.
        let small = fat32_geometry_for(|_cb| 10).expect("a tiny demand fits a FAT32 volume");
        assert_eq!(
            small.spc, 1,
            "small folder stays on the 1-sector cluster band"
        );
        assert_eq!(small.fatsz, 741, "M0's kernel-formula FAT size");
        assert_eq!(small.count_of_clusters, 94_742, "M0's data-cluster count");
        assert_eq!(sectors_per_cluster(small.part_sectors), small.spc);
    }

    #[test]
    fn root_child_dotdot_points_at_cluster_zero() {
        // M2: per fatgen103 6.5, a directory whose parent is the root must encode
        // its `..` FstClus as 0, not the root's actual cluster (2).
        let root = scratch("dotdot");
        std::fs::create_dir_all(root.join("SUB")).unwrap();
        std::fs::write(root.join("SUB/A.TXT"), b"hi").unwrap();
        let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 10])];
        let mut tree = build_tree(&root, &sys);
        allocate(&mut tree).expect("small folder fits a FAT32 volume");

        let sub = &tree.root.subdirs[0].dir;
        // `parent_first_cluster` still records the real parent (root = 2) for M2's
        // write engine; only the emitted `..` entry collapses it to 0.
        assert_eq!(sub.parent_first_cluster, ROOT_CLUSTER);

        // SUB sector 0: entry 0 = `.`, entry 1 = `..` (offset 32). Decode `..`'s
        // FstClusHI@0x14 + FstClusLO@0x1A from the on-disk bytes.
        let subsec = dir_sector(sub, false, 0);
        assert_eq!(&subsec[32..43], b"..         ", "entry 1 is ..");
        let hi = u16::from_le_bytes([subsec[32 + 0x14], subsec[32 + 0x15]]);
        let lo = u16::from_le_bytes([subsec[32 + 0x1A], subsec[32 + 0x1B]]);
        let cluster = (u32::from(hi) << 16) | u32::from(lo);
        assert_eq!(
            cluster, 0,
            "root-child `..` FstClus must be 0, not the root's 2"
        );

        // The `.` entry (offset 0) still names the subdir's own cluster, unaffected.
        let dot_hi = u16::from_le_bytes([subsec[0x14], subsec[0x15]]);
        let dot_lo = u16::from_le_bytes([subsec[0x1A], subsec[0x1B]]);
        let dot_cluster = (u32::from(dot_hi) << 16) | u32::from(dot_lo);
        assert_eq!(
            dot_cluster, sub.first_cluster,
            "`.` names the subdir itself"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
