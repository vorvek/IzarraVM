//! Lazy, read-only, whole-disk FAT32 volume view for the "Katea" storage
//! controller. This reproduces — byte for byte — the partitioned FAT32 disk that
//! `scripts/build-freedos-hdd-image.py` materializes into
//! `crates/izarravm-firmware/roms/tokados-hdd.img` (the image the real FreeDOS
//! kernel boots through the GO/NO-GO gate), but it serves file *data* lazily from
//! a host folder instead of holding the whole disk in RAM.
//!
//! Why a separate module from `fat32_volume.rs`? Two reasons:
//!   1. **The FAT size MUST match the kernel, not fatgen103.** `fat32.rs`'s
//!      `fat_size_sectors` uses the Microsoft fatgen103 formula, which yields 746
//!      sectors for this volume. The FreeDOS kernel's `CalculateFATData`
//!      (initdisk.c) yields **741**. The kernel computes a *default* BPB from the
//!      partition size and trusts it until `bldbpb` reads our on-disk VBR; if the
//!      on-disk FAT geometry disagrees with that default, the two views of the
//!      data region diverge and the kernel panics. 746-vs-741 was a real,
//!      gate-blocking bug, so this module re-derives the kernel formula and never
//!      touches `fat32::fat32_geometry`.
//!   2. **It must survive a 500 GB host folder.** `fat32_volume.rs` slurps every
//!      file into RAM (`fs::read`) at build time. This module stores only a path
//!      and a length for host files and reads the exact 512-byte span on demand in
//!      `read_sector`, so memory stays bounded regardless of folder size.
//!
//! The volume is the *whole disk*: LBA 0 is the MBR (with the partition entry
//! stamped), the FAT32 partition begins at `PART_START`, and everything past the
//! data region (or any unmapped sector) reads back as zeros. It is strictly
//! read-only; guest writes are not synced anywhere.

use crate::fat32::{FAT32_EOC, fat32_dir_entry, fat32_fsinfo_sector};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

/// One disk sector. The whole layout (BPB, FATs, directory entries) assumes
/// 512-byte sectors, matching the Python builder and `AtaDisk`.
pub(crate) const SECTOR: usize = 512;

// --- whole-disk + partition geometry (mirror build-freedos-hdd-image.py) ------

/// `AtaDisk`'s fixed head count; drives the MBR partition-entry CHS fields.
pub(crate) const HEADS: u32 = 16;
/// `AtaDisk`'s fixed sectors-per-track; drives the CHS fields and the BPB.
pub(crate) const SPT: u32 = 63;
/// 1 MiB-aligned partition start LBA (also the BPB HiddSec). The partition's VBR
/// stores disk-absolute LBAs because HiddSec carries this offset.
pub(crate) const PART_START: u32 = 2048;
/// 48 MiB disk: large enough for a valid FAT32 (>= 65525 clusters at 1
/// sector/cluster), small enough to be cheap. The partition fills the rest.
const DISK_SECTORS: u32 = 48 * 1024 * 1024 / SECTOR as u32; // 98304
/// The single partition spans the disk after the 1 MiB alignment gap.
const PART_SECTORS: u32 = DISK_SECTORS - PART_START; // 96256

// --- FAT32 BPB constants (mirror the Python builder, which mirrors fat32.rs) --

pub(crate) const RESERVED_SECTORS: u16 = 32;
pub(crate) const NUM_FATS: u8 = 2;
pub(crate) const ROOT_CLUSTER: u32 = 2;
pub(crate) const FSINFO_SECTOR: u16 = 1;
pub(crate) const BACKUP_BOOT_SECTOR: u16 = 6;
/// FreeDOS writes the backup FSInfo right after the backup boot record (+7).
pub(crate) const BACKUP_FSINFO_SECTOR: u16 = 7;
/// MBR partition type for a FAT32 partition addressed via LBA (INT 13h ext).
pub(crate) const PART_TYPE_FAT32_LBA: u8 = 0x0C;
/// A regular file's directory-entry attribute (archive bit). The Python builder
/// stamps 0x20 on every file.
pub(crate) const ATTR_ARCHIVE: u8 = 0x20;
/// FAT[0] = media descriptor 0xF8 (fixed disk) in the low byte, ones above.
pub(crate) const FAT0_MEDIA: u32 = 0x0FFF_FFF8;

/// Where a file's bytes live. Small system files are held in RAM; user files are
/// referenced by path and read on demand so a huge host folder never lands in
/// memory all at once.
#[derive(Debug)]
pub enum FileSource {
    /// Bytes held in RAM (KERNEL.SYS, COMMAND.COM, CONFIG.SYS, AUTOEXEC.BAT).
    InMemory(Vec<u8>),
    /// A host file: only its path and length are stored at construction. The
    /// length comes from `fs::metadata`; the bytes are read lazily, one 512-byte
    /// span at a time, inside `read_sector`.
    HostFile { path: PathBuf, len: u64 },
}

impl FileSource {
    /// The file's length in bytes, without reading its contents.
    pub(crate) fn len(&self) -> u64 {
        match self {
            FileSource::InMemory(v) => v.len() as u64,
            FileSource::HostFile { len, .. } => *len,
        }
    }
}

/// One root-directory file: a pre-validated 8.3 name (11 logical chars,
/// uppercase, e.g. "KERNEL.SYS") and where its bytes come from.
#[derive(Debug)]
pub struct VolumeFile {
    /// An 8.3 name such as "KERNEL.SYS" — caller guarantees it is valid and
    /// uppercase. Folded to the 11-byte directory field at construction.
    pub name: String,
    pub source: FileSource,
}

/// A file resolved into the data region: its source and the cluster range
/// `[first_cluster, first_cluster + cluster_count)` it occupies. The 8.3 name is
/// not kept here — it only ever feeds the root-directory entry, built at
/// construction.
#[derive(Debug)]
struct MappedFile {
    source: FileSource,
    size: u32,
    first_cluster: u32,
    cluster_count: u32,
}

/// `sectors_per_cluster` per the fatgen103 DskTableFAT32 cutoffs — the same table
/// `build-freedos-hdd-image.py::sectors_per_cluster` and `fat32.rs` use. Re-derived
/// here (rather than calling `fat32.rs`, whose function is private) so this module
/// owns its geometry. Panics below the FAT32 floor; the 48 MiB partition is well
/// above it (it lands on the 1-sector cluster branch).
pub(crate) fn sectors_per_cluster(total_sectors: u32) -> u8 {
    match total_sectors {
        0..=66_600 => panic!("partition too small for FAT32"),
        66_601..=532_480 => 1,
        532_481..=16_777_216 => 8,
        16_777_217..=33_554_432 => 16,
        33_554_433..=67_108_864 => 32,
        _ => 64,
    }
}

/// FAT size in sectors per the FreeDOS kernel's `CalculateFATData` (initdisk.c),
/// the SAME formula as the Python builder's `fat_size_sectors`. This is the
/// load-bearing difference from `fat32.rs`: the divisor is
/// `(bytes/sector / 4) * spc + nfats`, NOT fatgen103's `(256*spc + nfats) / 2`.
/// For this 96256-sector partition that gives **741** (fatgen103 gives 746, which
/// does not boot).
pub(crate) fn fat_size_sectors(total_sectors: u32, spc: u8) -> u32 {
    let fatdata = total_sectors - u32::from(RESERVED_SECTORS);
    let fatentpersec = SECTOR as u32 / 4; // 128 FAT32 entries per sector
    let divisor = fatentpersec * u32::from(spc) + u32::from(NUM_FATS);
    (fatdata + (2 * u32::from(spc) + divisor - 1)) / divisor
}

/// Pack an LBA into the 3-byte CHS field of an MBR partition entry, using the
/// 16x63 geometry `AtaDisk` derives. Cylinders above 1023 clamp to the all-ones
/// `0xFE 0xFF 0xFF` "use LBA" marker — the standard MBR convention and exactly
/// what the Python `lba_to_chs` does.
pub(crate) fn lba_to_chs(lba: u32) -> [u8; 3] {
    let cyl = lba / (HEADS * SPT);
    let rem = lba % (HEADS * SPT);
    let head = rem / SPT;
    let sect = rem % SPT + 1;
    if cyl > 1023 {
        return [0xFE, 0xFF, 0xFF];
    }
    let c_hi = (cyl >> 8) & 0x03;
    [
        (head & 0xFF) as u8,
        (((c_hi << 6) | (sect & 0x3F)) & 0xFF) as u8,
        (cyl & 0xFF) as u8,
    ]
}

/// A lazy, read-only, whole-disk FAT32 volume answered one 512-byte sector at a
/// time. Construction builds metadata only — it never reads host file contents.
///
/// ponytail: there is no page cache. The struct holds only the stamped boot
/// sectors (in RAM, they are tiny), the in-memory file blobs the caller passed,
/// and `{path, len}` for host files. A data-region read of a `HostFile` opens the
/// file, seeks, and reads exactly 512 bytes — so memory is bounded by the
/// in-memory files alone, even for a 500 GB host folder. A small LRU could be
/// added later if profiling shows the per-sector `open`+`seek` hurts; M0 does not
/// need it.
#[derive(Debug)]
pub struct KateaVolume {
    /// LBA 0: the MBR with the partition entry and 0x55AA stamped in.
    mbr: [u8; SECTOR],
    /// The FAT32 VBR (at PART_START) with the BPB stamped over the boot code.
    vbr: [u8; SECTOR],
    /// FSInfo free-cluster count, used at both FSInfo sectors.
    free_count: u32,
    /// FSInfo next-free hint, used at both FSInfo sectors.
    next_free: u32,
    /// `sectors_per_cluster`; 1 for this volume.
    spc: u8,
    /// One FAT's length in sectors (BPB_FATSz32), the kernel's 741 here.
    fatsz: u32,
    /// First data sector, partition-relative: `RESERVED_SECTORS + NUM_FATS * fatsz`.
    /// Cluster 2 begins here.
    first_data_sector: u32,
    /// One serialized FAT (`fatsz * 512` bytes), mirrored across both FAT copies.
    /// Built at construction from the cluster chains; the FAT is metadata-sized
    /// (here ~370 KiB) regardless of how large the host files are.
    fat_bytes: Vec<u8>,
    /// The root directory's contents (cluster 2): one 32-byte entry per file.
    root_dir: Vec<u8>,
    /// Files in allocation order, each tagged with its cluster range.
    files: Vec<MappedFile>,
}

impl KateaVolume {
    /// Build a whole-disk FAT32 view from the boot sectors and an ordered list of
    /// root-directory files. `mbr` and `vbr` are each a 512-byte image (the MBR
    /// may already carry boot code — only the partition entry and signature are
    /// stamped; the VBR's boot code is kept and only the BPB is stamped). The
    /// files are laid down in the given order starting at cluster 3 (cluster 2 is
    /// the root directory), exactly as the Python builder does.
    ///
    /// This reads no host file *contents*; `HostFile` entries carry only a path
    /// and length supplied by the caller (via `fs::metadata`, never `fs::read`).
    pub fn new(mbr: &[u8; SECTOR], vbr: &[u8; SECTOR], files: Vec<VolumeFile>) -> Self {
        let spc = sectors_per_cluster(PART_SECTORS);
        let fatsz = fat_size_sectors(PART_SECTORS, spc);
        let used = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fatsz;
        let first_data_sector = used;
        let cluster_bytes = u32::from(spc) * SECTOR as u32;
        let count_of_clusters = (PART_SECTORS - used) / u32::from(spc);

        // --- MBR: stamp the single partition entry + signature over the given
        // boot code, exactly as the Python builder's partition-table block. ----
        let mut mbr_out = *mbr;
        let pe = 0x1BE; // first partition entry
        mbr_out[pe] = 0x80; // active / bootable
        mbr_out[pe + 1..pe + 4].copy_from_slice(&lba_to_chs(PART_START));
        mbr_out[pe + 4] = PART_TYPE_FAT32_LBA;
        mbr_out[pe + 5..pe + 8].copy_from_slice(&lba_to_chs(PART_START + PART_SECTORS - 1));
        mbr_out[pe + 8..pe + 12].copy_from_slice(&PART_START.to_le_bytes()); // RelSect
        mbr_out[pe + 12..pe + 16].copy_from_slice(&PART_SECTORS.to_le_bytes()); // NumSect
        mbr_out[0x1FE] = 0x55;
        mbr_out[0x1FF] = 0xAA;

        // --- VBR: stamp the FAT32 BPB over the boot code, keeping the boot code.
        let mut vbr_out = *vbr;
        stamp_fat32_bpb(&mut vbr_out, spc, fatsz, PART_START, PART_SECTORS);
        assert!(
            vbr_out[0x1FE] == 0x55 && vbr_out[0x1FF] == 0xAA,
            "VBR boot signature missing"
        );

        // --- allocate cluster chains, build the FAT and the root directory -----
        // The FAT is sized to clusters 0..(count_of_clusters + 2); a chained file
        // walks c -> c+1 ending in EOC. Files get sequential clusters from 3 in
        // the given order — the root directory itself is cluster 2 (EOC).
        let mut fat = vec![0u8; fatsz as usize * SECTOR];
        let set_fat = |fat: &mut [u8], cluster: u32, value: u32| {
            let i = cluster as usize * 4;
            // The FAT region is sized to hold every entry; guard regardless so a
            // miscount can never index out of bounds.
            if let Some(slot) = fat.get_mut(i..i + 4) {
                slot.copy_from_slice(&(value & 0x0FFF_FFFF).to_le_bytes());
            }
        };
        set_fat(&mut fat, 0, FAT0_MEDIA);
        set_fat(&mut fat, 1, FAT32_EOC);
        set_fat(&mut fat, ROOT_CLUSTER, FAT32_EOC); // root is one cluster

        let mut next_free = ROOT_CLUSTER + 1; // cluster 3 is the first file
        let mut root_dir = Vec::with_capacity(files.len() * 32);
        let mut mapped = Vec::with_capacity(files.len());

        // The root directory is a single cluster here (matching the static image),
        // so it holds cluster_bytes/32 entries. Files past that, past the disk's
        // cluster count, or larger than FAT32's 4 GiB limit are dropped with a
        // warning rather than panicking or silently vanishing from DIR. ponytail: a
        // multi-cluster root and a folder-sized disk are Milestone-1 work; M0 only
        // needs to prove the read path, so a bounded, loud cap is enough.
        let max_root_entries = cluster_bytes as usize / 32;
        for f in files {
            let Ok(size) = u32::try_from(f.source.len()) else {
                eprintln!(
                    "katea: skipping {} (>= 4 GiB, not FAT32-representable)",
                    f.name
                );
                continue;
            };
            // At least one cluster even for an empty file: the Python builder uses
            // max(1, ceil(len/cluster_bytes)) and stamps a real first cluster.
            let nclu = (size.div_ceil(cluster_bytes)).max(1);
            let first = next_free;
            if root_dir.len() / 32 >= max_root_entries {
                eprintln!(
                    "katea: root directory full ({max_root_entries} entries); dropping {} and any further files",
                    f.name
                );
                break;
            }
            if first + nclu - 1 > count_of_clusters + 1 {
                eprintln!(
                    "katea: disk full; dropping {} and any further files",
                    f.name
                );
                break;
            }
            // Chain c -> c+1, last -> EOC.
            for i in 0..nclu {
                let c = first + i;
                let v = if i == nclu - 1 { FAT32_EOC } else { c + 1 };
                set_fat(&mut fat, c, v);
            }
            next_free += nclu;

            // 32-byte directory entry: 8.3 name, archive attr, split cluster, size.
            let name11 = fold_83(&f.name);
            let entry = fat32_dir_entry(&name11, ATTR_ARCHIVE, first, 0, 0, size);
            root_dir.extend_from_slice(&entry);

            mapped.push(MappedFile {
                source: f.source,
                size,
                first_cluster: first,
                cluster_count: nclu,
            });
        }

        // --- FSInfo hints, computed after allocation -------------------------
        let used_clusters = next_free - ROOT_CLUSTER; // clusters 2..next_free-1
        let free_count = count_of_clusters - used_clusters;

        Self {
            mbr: mbr_out,
            vbr: vbr_out,
            free_count,
            next_free,
            spc,
            fatsz,
            first_data_sector,
            fat_bytes: fat,
            root_dir,
            files: mapped,
        }
    }

    /// The whole-disk sector count (DISK_SECTORS, 98304 for the 48 MiB disk), so
    /// Task 4 can derive the ATA geometry it reports.
    pub fn total_sectors(&self) -> u32 {
        DISK_SECTORS
    }

    /// Read one whole-disk sector by absolute LBA. Resolves entirely from
    /// in-memory metadata except for `HostFile` data, which is read on demand.
    /// Out-of-range or unmapped sectors read back as zeros.
    pub fn read_sector(&self, lba: u32) -> [u8; SECTOR] {
        // LBA 0: the MBR.
        if lba == 0 {
            return self.mbr;
        }
        // Below the partition (the 1 MiB alignment gap) reads as zeros, matching
        // the Python image where that region is never written.
        if lba < PART_START {
            return [0u8; SECTOR];
        }
        let rel = lba - PART_START; // partition-relative sector

        // Reserved area: VBR (0), FSInfo (1), backup boot (6), backup FSInfo (7).
        if rel == 0 || rel == u32::from(BACKUP_BOOT_SECTOR) {
            return self.vbr;
        }
        if rel == u32::from(FSINFO_SECTOR) || rel == u32::from(BACKUP_FSINFO_SECTOR) {
            return fat32_fsinfo_sector(self.free_count, self.next_free);
        }

        // FAT region: NUM_FATS identical copies, each `fatsz` long, starting at
        // RESERVED_SECTORS.
        let reserved = u32::from(RESERVED_SECTORS);
        let fat_end = reserved + u32::from(NUM_FATS) * self.fatsz;
        if (reserved..fat_end).contains(&rel) {
            let within = (rel - reserved) % self.fatsz;
            return self.fat_slice(within as usize);
        }

        // Data region: cluster 2 begins at `first_data_sector`.
        if rel >= self.first_data_sector {
            let data_lba = rel - self.first_data_sector;
            let spc = u32::from(self.spc);
            let cluster = ROOT_CLUSTER + data_lba / spc;
            let sector_in_cluster = data_lba % spc;
            return self.read_data_sector(cluster, sector_in_cluster);
        }

        // Anything else (e.g. reserved gaps between known sectors) is zero.
        [0u8; SECTOR]
    }

    /// One sector of a FAT copy, zero-padded past the serialized entries.
    fn fat_slice(&self, sector: usize) -> [u8; SECTOR] {
        let mut out = [0u8; SECTOR];
        let off = sector * SECTOR;
        if let Some(slice) = self.fat_bytes.get(off..off + SECTOR) {
            out.copy_from_slice(slice);
        }
        out
    }

    /// Resolve one data-region sector: cluster 2 is the root directory; clusters
    /// 3.. belong to the files in allocation order. Returns zeros for clusters
    /// past the last file (free space).
    fn read_data_sector(&self, cluster: u32, sector_in_cluster: u32) -> [u8; SECTOR] {
        // Cluster 2: the root directory.
        if cluster == ROOT_CLUSTER {
            let off = (sector_in_cluster as usize) * SECTOR;
            let mut out = [0u8; SECTOR];
            // The root dir is shorter than a cluster; copy what exists, zero-pad.
            if let Some(end) = off.checked_add(SECTOR) {
                let lo = off.min(self.root_dir.len());
                let hi = end.min(self.root_dir.len());
                out[..hi - lo].copy_from_slice(&self.root_dir[lo..hi]);
            }
            return out;
        }

        // A file's clusters. Find the file owning this cluster, then map the
        // sector to a byte offset within the file.
        let cluster_bytes = u32::from(self.spc) * SECTOR as u32;
        for f in &self.files {
            let last = f.first_cluster + f.cluster_count;
            if (f.first_cluster..last).contains(&cluster) {
                let cluster_in_file = cluster - f.first_cluster;
                let byte_off = u64::from(cluster_in_file) * u64::from(cluster_bytes)
                    + u64::from(sector_in_cluster) * SECTOR as u64;
                return read_source_span(&f.source, byte_off, f.size);
            }
        }
        // Beyond the allocated clusters: free space.
        [0u8; SECTOR]
    }
}

/// The system payload pulled back out of a whole-disk FAT32 image: the two boot
/// sectors and the root files in directory order. Feeding `mbr`/`vbr` and the
/// files (as `InMemory`) back into `KateaVolume::new` reproduces the image.
#[derive(Debug)]
pub struct SystemPayload {
    /// LBA 0: the MBR (with its partition entry and 0x55AA signature).
    pub mbr: [u8; SECTOR],
    /// The FAT32 VBR at `PART_START`.
    pub vbr: [u8; SECTOR],
    /// Root-directory files in directory order as `(8.3 name, contents)`.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Pull the system payload back out of a whole-disk FAT32 image laid out exactly
/// like `tokados-hdd.img` — the inverse of `KateaVolume::new`. Returns the MBR
/// (LBA 0), the partition's VBR (the sector at `PART_START`), and the root files
/// in directory order.
///
/// This reads the BPB straight from the image rather than trusting the module
/// constants, so a malformed or unexpected image surfaces as a panic here rather
/// than a silent mismatch: it reads RESERVED, NUM_FATS, FATSz32, RootClus, and
/// spc out of the VBR and walks the on-disk FAT chains, concatenating cluster
/// bytes truncated to each entry's file size. LFN (attr 0x0F), volume-label
/// (attr bit 0x08), free (0x00), and deleted (0xE5) entries are skipped.
///
/// Panics only on a truncated/garbled image (the embedded one is well formed, and
/// the round-trip test guards regressions).
pub fn extract_system_payload(image: &[u8]) -> SystemPayload {
    let sector = |lba: u32| -> &[u8] {
        let off = lba as usize * SECTOR;
        image
            .get(off..off + SECTOR)
            .unwrap_or_else(|| panic!("katea: image too short for LBA {lba}"))
    };
    let le16 = |s: &[u8], at: usize| u16::from_le_bytes([s[at], s[at + 1]]);
    let le32 = |s: &[u8], at: usize| u32::from_le_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]]);

    let mut mbr = [0u8; SECTOR];
    mbr.copy_from_slice(sector(0));

    // The partition start is in the MBR partition entry (RelSect); fall back to the
    // constant only as a sanity check — they must agree for our own image.
    let part_start = le32(&mbr, 0x1BE + 8);
    debug_assert_eq!(part_start, PART_START, "MBR partition start != PART_START");

    let mut vbr = [0u8; SECTOR];
    vbr.copy_from_slice(sector(part_start));

    // BPB fields, read from the on-disk VBR.
    let reserved = le16(&vbr, 0x0E);
    let num_fats = vbr[0x10];
    let fatsz = le32(&vbr, 0x24); // BPB_FATSz32
    let root_clus = le32(&vbr, 0x2C);
    let spc = vbr[0x0D];

    let first_data_sector = u32::from(reserved) + u32::from(num_fats) * fatsz;
    let spc32 = u32::from(spc);

    // The first FAT, partition-relative, used to follow cluster chains.
    let fat_base = part_start + u32::from(reserved);
    let fat_entry = |cluster: u32| -> u32 {
        let byte_off = cluster as usize * 4;
        let fat_sector = fat_base + (byte_off / SECTOR) as u32;
        let within = byte_off % SECTOR;
        le32(sector(fat_sector), within) & 0x0FFF_FFFF
    };
    // The absolute LBA of the first sector of a data cluster.
    let cluster_lba = |cluster: u32| part_start + first_data_sector + (cluster - root_clus) * spc32;
    // Concatenate a cluster chain's raw bytes, following c -> FAT[c] to EOC.
    let read_chain = |first: u32| -> Vec<u8> {
        // A cluster chain can't be longer than the disk has sectors; bound the walk
        // so a cyclic or corrupt FAT (this fn is pub) can't loop forever.
        let max_clusters = image.len() / SECTOR;
        let mut out = Vec::new();
        let mut c = first;
        for _ in 0..max_clusters {
            for s in 0..spc32 {
                out.extend_from_slice(sector(cluster_lba(c) + s));
            }
            let next = fat_entry(c);
            if next >= 0x0FFF_FFF8 {
                return out;
            }
            c = next;
        }
        panic!("katea: cluster chain from {first} exceeds the disk; corrupt FAT")
    };

    // Walk the root directory (a cluster chain starting at RootClus), parsing
    // 32-byte entries.
    let root_bytes = read_chain(root_clus);
    let mut files = Vec::new();
    for entry in root_bytes.chunks_exact(32) {
        match entry[0] {
            0x00 => break,    // no further entries in this directory
            0xE5 => continue, // deleted
            _ => {}
        }
        let attr = entry[11];
        if attr == 0x0F || attr & 0x08 != 0 {
            // LFN fragment or the volume label: not a file.
            continue;
        }
        let name = decode_83(&entry[0..11]);
        let first = (le16(entry, 0x14) as u32) << 16 | le16(entry, 0x1A) as u32;
        let size = le32(entry, 0x1C);
        let mut data = read_chain(first);
        data.truncate(size as usize);
        files.push((name, data));
    }

    SystemPayload { mbr, vbr, files }
}

/// Decode an 11-byte 8.3 directory field ("KERNEL  SYS") into "KERNEL.SYS" — the
/// inverse of `fold_83`. A blank extension yields just the base name. Re-folding
/// the result through `fold_83` reproduces the original 11 bytes exactly.
pub(crate) fn decode_83(raw: &[u8]) -> String {
    let base = String::from_utf8_lossy(&raw[0..8]).trim_end().to_string();
    let ext = String::from_utf8_lossy(&raw[8..11]).trim_end().to_string();
    if ext.is_empty() {
        base
    } else {
        format!("{base}.{ext}")
    }
}

/// Read exactly one 512-byte span at `byte_off` from a file's source, zero-padding
/// the tail past `size`. For `HostFile`, this opens, seeks, and reads on demand —
/// no whole-file slurp.
fn read_source_span(source: &FileSource, byte_off: u64, size: u32) -> [u8; SECTOR] {
    let mut out = [0u8; SECTOR];
    // The portion of this sector that lies within the file (the rest is the
    // cluster's slack, which must read as zeros).
    let valid = u64::from(size).saturating_sub(byte_off).min(SECTOR as u64) as usize;
    if valid == 0 {
        return out;
    }
    match source {
        FileSource::InMemory(v) => {
            let start = byte_off as usize;
            out[..valid].copy_from_slice(&v[start..start + valid]);
        }
        FileSource::HostFile { path, .. } => {
            // ponytail: open per read. Bounded memory beats a cache for M0; the
            // per-sector open is the simplest thing that is correct.
            match File::open(path).and_then(|mut f| {
                f.seek(SeekFrom::Start(byte_off))?;
                f.read_exact(&mut out[..valid])
            }) {
                Ok(()) => {}
                Err(e) => {
                    // A host file that vanished or shrank mid-run reads as zeros
                    // rather than panicking the guest; surface it for debugging.
                    eprintln!("katea: read {} @ {byte_off}: {e}", path.display());
                    out = [0u8; SECTOR];
                }
            }
        }
    }
    out
}

/// Fold a caller-supplied 8.3 name like "KERNEL.SYS" into the canonical 11-byte
/// directory field (8 + 3, space-padded, uppercase) — the same packing as the
/// Python builder's `name11`. The caller guarantees a valid uppercase 8.3 name,
/// so this only splits on the dot and pads; it does no character scrubbing.
fn fold_83(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = match name.split_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    debug_assert!(base.len() <= 8 && ext.len() <= 3, "name not 8.3: {name}");
    let base = base.as_bytes();
    let ext = ext.as_bytes();
    out[..base.len().min(8)].copy_from_slice(&base[..base.len().min(8)]);
    out[8..8 + ext.len().min(3)].copy_from_slice(&ext[..ext.len().min(3)]);
    out
}

/// Stamp the FAT32 BPB over `vbr`, keeping its boot code. Field offsets/values
/// mirror the Python builder's `stamp_fat32_bpb` exactly: HiddSec is `part_start`
/// (this is a partition, not a superfloppy), BS_DrvNum is 0x80, the label is
/// "TOKA-DOS   " and the serial 0x32303236. All multi-byte fields little-endian.
/// `part_start` (HiddSec) and `part_sectors` (TotSec32) are passed so the
/// dynamically-sized tree volume can reuse the same stamp.
pub(crate) fn stamp_fat32_bpb(
    vbr: &mut [u8; SECTOR],
    spc: u8,
    fatsz: u32,
    part_start: u32,
    part_sectors: u32,
) {
    vbr[0x03..0x0B].copy_from_slice(b"MSWIN4.1"); // OEM (fatgen103 recommendation)
    vbr[0x0B..0x0D].copy_from_slice(&(SECTOR as u16).to_le_bytes()); // bytes/sector
    vbr[0x0D] = spc; // sectors/cluster
    vbr[0x0E..0x10].copy_from_slice(&RESERVED_SECTORS.to_le_bytes());
    vbr[0x10] = NUM_FATS;
    vbr[0x11..0x13].copy_from_slice(&0u16.to_le_bytes()); // RootEntCnt: 0 on FAT32
    vbr[0x13..0x15].copy_from_slice(&0u16.to_le_bytes()); // TotSec16: 0 on FAT32
    vbr[0x15] = 0xF8; // media: fixed disk
    vbr[0x16..0x18].copy_from_slice(&0u16.to_le_bytes()); // FATSz16: 0 on FAT32
    vbr[0x18..0x1A].copy_from_slice(&(SPT as u16).to_le_bytes()); // sectors/track (cosmetic)
    vbr[0x1A..0x1C].copy_from_slice(&(HEADS as u16).to_le_bytes()); // heads (cosmetic)
    vbr[0x1C..0x20].copy_from_slice(&part_start.to_le_bytes()); // HiddSec = partition start
    vbr[0x20..0x24].copy_from_slice(&part_sectors.to_le_bytes()); // TotSec32
    // FAT32 extended BPB.
    vbr[0x24..0x28].copy_from_slice(&fatsz.to_le_bytes()); // BPB_FATSz32
    vbr[0x28..0x2A].copy_from_slice(&0u16.to_le_bytes()); // ExtFlags: mirroring active
    vbr[0x2A..0x2C].copy_from_slice(&0u16.to_le_bytes()); // FSVer 0.0
    vbr[0x2C..0x30].copy_from_slice(&ROOT_CLUSTER.to_le_bytes()); // RootClus
    vbr[0x30..0x32].copy_from_slice(&FSINFO_SECTOR.to_le_bytes());
    vbr[0x32..0x34].copy_from_slice(&BACKUP_BOOT_SECTOR.to_le_bytes());
    vbr[0x40] = 0x80; // BS_DrvNum (boot32lb stores DL here too)
    vbr[0x42] = 0x29; // BS_BootSig
    vbr[0x43..0x47].copy_from_slice(&0x3230_3236u32.to_le_bytes()); // BS_VolID
    vbr[0x47..0x52].copy_from_slice(b"TOKA-DOS   "); // BS_VolLab (11)
    vbr[0x52..0x5A].copy_from_slice(b"FAT32   "); // BS_FilSysType (8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn le16(s: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([s[at], s[at + 1]])
    }
    fn le32(s: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]])
    }

    /// A synthetic 512-byte MBR carrying only the boot signature; the rest is the
    /// caller's choice (here a recognizable filler so the partition stamp shows).
    fn synthetic_mbr() -> [u8; 512] {
        let mut m = [0xCCu8; 512];
        m[510] = 0x55;
        m[511] = 0xAA;
        m
    }

    /// A synthetic 512-byte VBR: zero body with the boot signature, so we can see
    /// the BPB stamp land on a clean slate.
    fn synthetic_vbr() -> [u8; 512] {
        let mut v = [0u8; 512];
        v[510] = 0x55;
        v[511] = 0xAA;
        v
    }

    /// Unique scratch dir under the system temp dir; cleaned by the caller.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "katea_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Construct a `HostFile` source from a path, reading only its metadata.
    fn host_file(path: PathBuf) -> FileSource {
        let len = std::fs::metadata(&path).unwrap().len();
        FileSource::HostFile { path, len }
    }

    /// Read a whole file back through the FAT chain via `read_sector`, by
    /// following `c -> c+1` links in the serialized FAT (sequential here).
    fn read_back(vol: &KateaVolume, first_cluster: u32, size: u32) -> Vec<u8> {
        let spc = u32::from(vol.spc);
        let mut out = Vec::new();
        let mut cl = first_cluster;
        // Walk the chain through the serialized FAT, stopping at the EOC marker.
        loop {
            for s in 0..spc {
                let lba = PART_START + vol.first_data_sector + (cl - ROOT_CLUSTER) * spc + s;
                out.extend_from_slice(&vol.read_sector(lba));
            }
            let next = le32(&vol.fat_bytes, cl as usize * 4) & 0x0FFF_FFFF;
            if next >= 0x0FFF_FFF8 {
                break;
            }
            cl = next;
        }
        out.truncate(size as usize);
        out
    }

    #[test]
    fn mbr_partition_entry_is_stamped() {
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), Vec::new());
        let mbr = vol.read_sector(0);
        let pe = 0x1BE;
        assert_eq!(mbr[pe], 0x80, "active flag");
        assert_eq!(mbr[pe + 4], 0x0C, "FAT32-LBA partition type");
        assert_eq!(le32(&mbr, pe + 8), 2048, "RelSect = PART_START");
        assert_eq!(le32(&mbr, pe + 12), 96256, "NumSect = PART_SECTORS");
        assert_eq!(&mbr[pe + 1..pe + 4], &[0x00, 0x21, 0x02], "CHS start");
        assert_eq!(&mbr[pe + 5..pe + 8], &[0x08, 0x18, 0x61], "CHS end");
        assert_eq!(&mbr[510..512], &[0x55, 0xAA], "MBR signature");
        // The given boot code outside the stamped fields survives.
        assert_eq!(mbr[0], 0xCC, "boot code byte 0 preserved");
        assert_eq!(
            mbr[0x1BD], 0xCC,
            "byte just before the partition table preserved"
        );
    }

    #[test]
    fn vbr_has_the_fat32_bpb() {
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), Vec::new());
        let vbr = vol.read_sector(2048);
        assert_eq!(le16(&vbr, 0x0B), 512, "bytes/sector");
        assert_eq!(vbr[0x0D], 1, "sectors/cluster = 1");
        assert_eq!(le16(&vbr, 0x0E), 32, "reserved sectors");
        assert_eq!(vbr[0x10], 2, "num FATs");
        assert_eq!(le32(&vbr, 0x1C), 2048, "HiddSec = PART_START");
        assert_eq!(le32(&vbr, 0x20), 96256, "TotSec32 = PART_SECTORS");
        assert_eq!(le32(&vbr, 0x24), 741, "BPB_FATSz32 = kernel formula 741");
        assert_eq!(le32(&vbr, 0x2C), 2, "RootClus = 2");
        assert_eq!(le16(&vbr, 0x30), 1, "FSInfo sector");
        assert_eq!(le16(&vbr, 0x32), 6, "backup boot sector");
        assert_eq!(vbr[0x40], 0x80, "BS_DrvNum");
        assert_eq!(vbr[0x42], 0x29, "BS_BootSig");
        assert_eq!(le32(&vbr, 0x43), 0x3230_3236, "BS_VolID");
        assert_eq!(&vbr[0x47..0x52], b"TOKA-DOS   ", "volume label");
        assert_eq!(&vbr[0x52..0x5A], b"FAT32   ", "filesystem type");
        assert_eq!(&vbr[510..512], &[0x55, 0xAA], "VBR signature");
        // The backup boot (+6) mirrors the VBR.
        assert_eq!(vol.read_sector(2048 + 6), vbr, "backup boot is a VBR copy");
    }

    #[test]
    fn fat_chains_match_the_two_files() {
        let dir = scratch("fatchain");
        let host_path = dir.join("DATA.BIN");
        std::fs::write(&host_path, vec![0x5A; 300]).unwrap(); // 1 cluster

        let files = vec![
            // 600 bytes at spc=1 (512-byte clusters) spans 2 clusters: 3 -> 4.
            VolumeFile {
                name: "SYSTEM.SYS".to_string(),
                source: FileSource::InMemory(vec![0xAB; 600]),
            },
            // DATA.BIN: 300 bytes -> 1 cluster: 5 -> EOC.
            VolumeFile {
                name: "DATA.BIN".to_string(),
                source: host_file(host_path),
            },
        ];
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), files);
        std::fs::remove_dir_all(&dir).ok();

        // FAT #1 is at partition + RESERVED_SECTORS = 2048 + 32.
        let fat = vol.read_sector(2048 + 32);
        assert_eq!(le32(&fat, 0), 0x0FFF_FFF8, "FAT[0] media + ones");
        assert_eq!(le32(&fat, 4), 0x0FFF_FFFF, "FAT[1] EOC");
        assert_eq!(le32(&fat, 2 * 4), 0x0FFF_FFFF, "FAT[2] root = EOC");
        assert_eq!(le32(&fat, 3 * 4), 4, "FAT[3] = 4 (SYSTEM.SYS cluster 1->2)");
        assert_eq!(
            le32(&fat, 4 * 4),
            0x0FFF_FFFF,
            "FAT[4] = EOC (SYSTEM.SYS end)"
        );
        assert_eq!(
            le32(&fat, 5 * 4),
            0x0FFF_FFFF,
            "FAT[5] = EOC (DATA.BIN, 1 cluster)"
        );
        // FAT #2 (a mirror) is at partition + 32 + fatsz (741).
        let fat2 = vol.read_sector(2048 + 32 + 741);
        assert_eq!(fat2, fat, "FAT #2 mirrors FAT #1");
    }

    #[test]
    fn root_directory_has_both_entries_in_order() {
        let dir = scratch("rootdir");
        let host_path = dir.join("DATA.BIN");
        std::fs::write(&host_path, vec![0x5A; 300]).unwrap();
        let files = vec![
            VolumeFile {
                name: "SYSTEM.SYS".to_string(),
                source: FileSource::InMemory(vec![0xAB; 600]),
            },
            VolumeFile {
                name: "DATA.BIN".to_string(),
                source: host_file(host_path),
            },
        ];
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), files);
        std::fs::remove_dir_all(&dir).ok();

        // Root dir is cluster 2: partition + first_data_sector + 0.
        let root = vol.read_sector(2048 + vol.first_data_sector);
        // Entry 0: SYSTEM.SYS, first cluster 3, size 600.
        assert_eq!(&root[0..11], b"SYSTEM  SYS", "first entry name");
        assert_eq!(root[11], 0x20, "archive attr");
        let cl0 = (u32::from(le16(&root, 0x14)) << 16) | u32::from(le16(&root, 0x1A));
        assert_eq!(cl0, 3, "SYSTEM.SYS first cluster");
        assert_eq!(le32(&root, 0x1C), 600, "SYSTEM.SYS size");
        // Entry 1: DATA.BIN, first cluster 5, size 300.
        assert_eq!(&root[32..43], b"DATA    BIN", "second entry name");
        let cl1 = (u32::from(le16(&root, 32 + 0x14)) << 16) | u32::from(le16(&root, 32 + 0x1A));
        assert_eq!(cl1, 5, "DATA.BIN first cluster");
        assert_eq!(le32(&root, 32 + 0x1C), 300, "DATA.BIN size");
    }

    #[test]
    fn host_file_data_reads_through_from_disk() {
        let dir = scratch("hostdata");
        let host_path = dir.join("DATA.BIN");
        // A position-derived pattern so a wrong offset is obvious.
        let payload: Vec<u8> = (0..300u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&host_path, &payload).unwrap();
        let files = vec![
            VolumeFile {
                name: "SYSTEM.SYS".to_string(),
                source: FileSource::InMemory(vec![0xAB; 600]),
            },
            VolumeFile {
                name: "DATA.BIN".to_string(),
                source: host_file(host_path),
            },
        ];
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), files);

        // SYSTEM.SYS (cluster 3) round-trips from RAM.
        assert_eq!(read_back(&vol, 3, 600), vec![0xAB; 600]);
        // DATA.BIN (cluster 5) round-trips from the host file on disk.
        assert_eq!(read_back(&vol, 5, 300), payload);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lazy_read_hits_the_end_of_a_large_host_file() {
        // ponytail: the volume stored only {path, len} for this 1 MiB file (no
        // slurp); reading a sector near the END proves the facade seeks into the
        // file on demand rather than holding it in RAM.
        let dir = scratch("lazy");
        let host_path = dir.join("BIG.DAT");
        let total: usize = 1024 * 1024;
        {
            let mut f = std::fs::File::create(&host_path).unwrap();
            let buf: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
            f.write_all(&buf).unwrap();
        }
        let files = vec![VolumeFile {
            name: "BIG.DAT".to_string(),
            source: host_file(host_path),
        }];
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), files);

        // BIG.DAT starts at cluster 3 = first_data_sector + (3-2)*1 sectors. Read
        // the LAST full 512-byte sector of the 1 MiB file (sector index 2047).
        let last_sector_index = (total / SECTOR) as u32 - 1; // 2047
        let lba = PART_START + vol.first_data_sector + (3 - ROOT_CLUSTER) + last_sector_index;
        let got = vol.read_sector(lba);
        let off = last_sector_index as usize * SECTOR;
        let expect: Vec<u8> = (off..off + SECTOR).map(|i| (i % 251) as u8).collect();
        assert_eq!(
            &got[..],
            &expect[..],
            "tail sector matches the pattern at offset"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn out_of_range_sector_is_zeros() {
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), Vec::new());
        // Past the whole disk.
        assert_eq!(vol.read_sector(DISK_SECTORS + 10), [0u8; 512]);
        // A free data cluster (no files) reads as zeros.
        let free = PART_START + vol.first_data_sector + 100;
        assert_eq!(vol.read_sector(free), [0u8; 512]);
        // The 1 MiB alignment gap below the partition is zeros.
        assert_eq!(vol.read_sector(100), [0u8; 512]);
    }

    #[test]
    fn total_sectors_is_the_whole_disk() {
        let vol = KateaVolume::new(&synthetic_mbr(), &synthetic_vbr(), Vec::new());
        assert_eq!(vol.total_sectors(), 98304, "48 MiB whole-disk sector count");
    }

    /// The extractor pulls the exact system payload back out of the committed,
    /// proven-bootable image: the five files at their known sizes/first bytes, and
    /// both boot sectors carrying the 0x55AA signature.
    #[test]
    fn extracts_the_embedded_image_payload() {
        let img = izarravm_firmware::tokados_hdd_img();
        let payload = extract_system_payload(img);

        assert_eq!(&payload.mbr[510..512], &[0x55, 0xAA], "MBR boot signature");
        assert_eq!(&payload.vbr[510..512], &[0x55, 0xAA], "VBR boot signature");

        // The files, in directory order, with their known sizes.
        let by_name: std::collections::HashMap<&str, &Vec<u8>> =
            payload.files.iter().map(|(n, d)| (n.as_str(), d)).collect();
        assert_eq!(
            by_name.get("KERNEL.SYS").map(|d| d.len()),
            Some(70130),
            "KERNEL.SYS size"
        );
        assert_eq!(
            by_name.get("COMMAND.COM").map(|d| d.len()),
            Some(87652),
            "COMMAND.COM size"
        );
        assert!(by_name.contains_key("CONFIG.SYS"), "CONFIG.SYS present");
        assert!(by_name.contains_key("AUTOEXEC.BAT"), "AUTOEXEC.BAT present");
        assert!(by_name.contains_key("HELLO.TXT"), "HELLO.TXT present");

        // The kernel signon points at "See C:\\LICENSE.TXT for more.", so the full
        // FreeDOS / Toka-DOS licensing ships as a real file on the C: payload.
        let license = by_name.get("LICENSE.TXT").expect("LICENSE.TXT present");
        assert!(
            String::from_utf8_lossy(license).contains("GNU GENERAL PUBLIC LICENSE"),
            "LICENSE.TXT carries the full GPL text"
        );

        // FreeDOS KERNEL.SYS is a raw binary, not an MZ: it begins with a short
        // JMP (0xEB) past the embedded BPB — the load-bearing first byte the boot
        // sector relies on.
        let kernel = by_name.get("KERNEL.SYS").unwrap();
        assert_eq!(kernel[0], 0xEB, "KERNEL.SYS begins with a short JMP");

        // The rebranded, trimmed signon banner is compiled into the kernel.
        let has = |needle: &str| kernel.windows(needle.len()).any(|w| w == needle.as_bytes());
        assert!(
            has("General Simulation Works"),
            "the rebranded signon company name is in the kernel"
        );
        assert!(
            !has("JTM Soluciones"),
            "the old company name was removed from the kernel"
        );
    }

    /// The definitive boot de-risk: extracting the committed image and rebuilding a
    /// `KateaVolume` from it reproduces the proven-bootable disk byte-for-byte,
    /// every sector. If the facade matches the image the kernel boots, then the
    /// facade boots. A mismatch reports the exact LBA and the first differing byte.
    #[test]
    fn facade_reproduces_the_bootable_image_byte_for_byte() {
        let img = izarravm_firmware::tokados_hdd_img();
        let payload = extract_system_payload(img);
        let volume_files = payload
            .files
            .into_iter()
            .map(|(name, data)| VolumeFile {
                name,
                source: FileSource::InMemory(data),
            })
            .collect();
        let vol = KateaVolume::new(&payload.mbr, &payload.vbr, volume_files);

        let total = vol.total_sectors();
        assert_eq!(
            img.len(),
            total as usize * SECTOR,
            "image length matches the whole-disk sector count"
        );
        for lba in 0..total {
            let got = vol.read_sector(lba);
            let off = lba as usize * SECTOR;
            let want = &img[off..off + SECTOR];
            if got != want {
                let first_diff = (0..SECTOR).find(|&i| got[i] != want[i]).unwrap();
                panic!(
                    "sector mismatch at LBA {lba}: first differing byte {first_diff} \
                     (facade={:#04x}, image={:#04x})",
                    got[first_diff], want[first_diff]
                );
            }
        }
    }
}
