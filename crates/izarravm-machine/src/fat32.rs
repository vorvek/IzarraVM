//! FAT32 volume geometry. Given a volume size, compute the BPB fields a
//! synthesized FAT32 volume needs: sectors per cluster, reserved sectors, the
//! size of one FAT, the usable cluster count, and the fixed FAT32 layout
//! sectors. The numbers follow Microsoft's FAT specification (fatgen103): the
//! DskTableFAT32 cluster-size table and the FATSz32 computation. A later slice
//! builds the actual boot sector, FATs, and directory tree from this geometry.

// The DskTableFAT32 cluster table and the FATSz32 math are only valid for
// 512-byte sectors (a fatgen103 precondition), so this stays 512.
const BYTES_PER_SECTOR: u16 = 512;
/// FAT32 reserves 32 sectors before the first FAT (fatgen103 default).
const RESERVED_SECTORS: u16 = 32;
const NUM_FATS: u8 = 2;
/// The root directory is an ordinary cluster chain starting at cluster 2.
const ROOT_CLUSTER: u32 = 2;
const FSINFO_SECTOR: u16 = 1;
const BACKUP_BOOT_SECTOR: u16 = 6;
/// FAT32 is valid only at or above this cluster count (fatgen103 3.5); below it
/// the volume would be FAT16 or FAT12.
const MIN_FAT32_CLUSTERS: u32 = 65525;

/// The computed geometry of a synthesized FAT32 volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fat32Geometry {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub total_sectors: u32,
    /// Sectors occupied by one FAT (BPB_FATSz32).
    pub fat_size_sectors: u32,
    pub root_cluster: u32,
    pub fsinfo_sector: u16,
    pub backup_boot_sector: u16,
    /// Usable data clusters (fatgen103 CountofClusters), not counting the two
    /// reserved FAT entries.
    pub count_of_clusters: u32,
    /// First sector of the data region; cluster 2 begins here.
    pub first_data_sector: u32,
}

/// Map a volume size in 512-byte sectors to sectors-per-cluster, per fatgen103's
/// DskTableFAT32. None means the volume is too small to format as FAT32.
fn sectors_per_cluster(total_sectors: u32) -> Option<u8> {
    Some(match total_sectors {
        0..=66_600 => return None,     // up to 32.5 MB: too small for FAT32
        66_601..=532_480 => 1,         // up to 260 MB: 512-byte clusters
        532_481..=16_777_216 => 8,     // up to 8 GB: 4 KiB clusters
        16_777_217..=33_554_432 => 16, // up to 16 GB: 8 KiB clusters
        33_554_433..=67_108_864 => 32, // up to 32 GB: 16 KiB clusters
        _ => 64,                       // larger: 32 KiB clusters
    })
}

/// Compute the FAT32 geometry for a volume of `volume_bytes`. Returns None when
/// the volume is too small to be a valid FAT32 (fewer than 65525 clusters) or
/// too large for a 32-bit sector count (FAT32 tops out near 2 TB).
pub fn fat32_geometry(volume_bytes: u64) -> Option<Fat32Geometry> {
    let total_sectors = u32::try_from(volume_bytes / u64::from(BYTES_PER_SECTOR)).ok()?;
    let spc = sectors_per_cluster(total_sectors)?;

    // FATSz32 per fatgen103. RootDirSectors is 0 on FAT32, so:
    //   tmp1 = TotSec - ReservedSectors
    //   tmp2 = ((256 * SecPerClus) + NumFATs) / 2
    //   FATSz = ceil(tmp1 / tmp2)
    // The spec notes this can overshoot by a few sectors but never undershoots.
    let tmp1 = total_sectors - u32::from(RESERVED_SECTORS);
    let tmp2 = ((256 * u32::from(spc)) + u32::from(NUM_FATS)) / 2;
    let fat_size_sectors = tmp1.div_ceil(tmp2);

    let used = u32::from(RESERVED_SECTORS) + u32::from(NUM_FATS) * fat_size_sectors;
    let data_sectors = total_sectors.checked_sub(used)?;
    let count_of_clusters = data_sectors / u32::from(spc);
    if count_of_clusters < MIN_FAT32_CLUSTERS {
        return None;
    }

    Some(Fat32Geometry {
        bytes_per_sector: BYTES_PER_SECTOR,
        sectors_per_cluster: spc,
        reserved_sectors: RESERVED_SECTORS,
        num_fats: NUM_FATS,
        total_sectors,
        fat_size_sectors,
        root_cluster: ROOT_CLUSTER,
        fsinfo_sector: FSINFO_SECTOR,
        backup_boot_sector: BACKUP_BOOT_SECTOR,
        count_of_clusters,
        first_data_sector: used,
    })
}

/// OEM name in the boot sector. fatgen103 recommends "MSWIN4.1" because some FAT
/// drivers check this field; the floppy path (fat12.rs) uses a house name, but
/// the FAT32 volume is meant to be read by arbitrary software, so it follows the
/// spec recommendation.
const FAT32_OEM_NAME: &[u8; 8] = b"MSWIN4.1";

/// Build the 512-byte FAT32 boot sector (sector 0) for `geo`, with `volume_id`
/// as the volume serial. No bootstrap code (a data volume), but it carries the
/// full FAT32 BPB and the 0x55AA signature so a FAT driver mounts it. All
/// multi-byte fields are little-endian, per fatgen103.
pub fn fat32_boot_sector(geo: &Fat32Geometry, volume_id: u32) -> [u8; 512] {
    let mut s = [0u8; 512];
    // JMP short to the boot code at 0x5A (the FAT32 BPB reaches 0x59), then NOP.
    s[0] = 0xeb;
    s[1] = 0x58;
    s[2] = 0x90;
    s[3..11].copy_from_slice(FAT32_OEM_NAME);
    // Common BPB (offsets 11..36).
    s[11..13].copy_from_slice(&geo.bytes_per_sector.to_le_bytes());
    s[13] = geo.sectors_per_cluster;
    s[14..16].copy_from_slice(&geo.reserved_sectors.to_le_bytes());
    s[16] = geo.num_fats;
    s[17..19].copy_from_slice(&0u16.to_le_bytes()); // RootEntCnt: 0 on FAT32
    s[19..21].copy_from_slice(&0u16.to_le_bytes()); // TotSec16: 0 on FAT32
    s[21] = 0xf8; // media descriptor: fixed disk
    s[22..24].copy_from_slice(&0u16.to_le_bytes()); // FATSz16: 0 on FAT32
    s[24..26].copy_from_slice(&63u16.to_le_bytes()); // sectors/track (CHS, cosmetic under LBA)
    s[26..28].copy_from_slice(&255u16.to_le_bytes()); // heads (CHS, cosmetic under LBA)
    s[28..32].copy_from_slice(&0u32.to_le_bytes()); // hidden sectors (whole volume, not a partition)
    s[32..36].copy_from_slice(&geo.total_sectors.to_le_bytes()); // TotSec32
    // FAT32 extended BPB (offsets 36..90).
    s[36..40].copy_from_slice(&geo.fat_size_sectors.to_le_bytes()); // BPB_FATSz32
    s[40..42].copy_from_slice(&0u16.to_le_bytes()); // BPB_ExtFlags: FAT mirroring active
    s[42..44].copy_from_slice(&0u16.to_le_bytes()); // BPB_FSVer 0.0
    s[44..48].copy_from_slice(&geo.root_cluster.to_le_bytes()); // BPB_RootClus
    s[48..50].copy_from_slice(&geo.fsinfo_sector.to_le_bytes()); // BPB_FSInfo
    s[50..52].copy_from_slice(&geo.backup_boot_sector.to_le_bytes()); // BPB_BkBootSec
    // s[52..64] BPB_Reserved stays zero.
    s[64] = 0x80; // BS_DrvNum: first hard disk
    s[65] = 0x00; // BS_Reserved1
    s[66] = 0x29; // BS_BootSig: the volume-id/label/type fields follow
    s[67..71].copy_from_slice(&volume_id.to_le_bytes()); // BS_VolID
    s[71..82].copy_from_slice(b"NO NAME    "); // BS_VolLab (11 bytes)
    s[82..90].copy_from_slice(b"FAT32   "); // BS_FilSysType (8 bytes)
    // s[90..510] boot code stays zero.
    s[510] = 0x55;
    s[511] = 0xaa;
    s
}

/// Build the 512-byte FAT32 FSInfo sector (BPB_FSInfo names its location, usually
/// sector 1). `free_count` is the last known free-cluster count and `next_free`
/// a hint for the next free cluster to allocate; 0xFFFFFFFF means "unknown" for
/// either (fatgen103 Section 5).
pub fn fat32_fsinfo_sector(free_count: u32, next_free: u32) -> [u8; 512] {
    let mut s = [0u8; 512];
    s[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes()); // FSI_LeadSig
    // s[4..484] FSI_Reserved1 stays zero.
    s[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes()); // FSI_StrucSig
    s[488..492].copy_from_slice(&free_count.to_le_bytes()); // FSI_Free_Count
    s[492..496].copy_from_slice(&next_free.to_le_bytes()); // FSI_Nxt_Free
    // s[496..508] FSI_Reserved2 stays zero.
    s[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes()); // FSI_TrailSig
    s
}

/// FAT32 end-of-chain marker written for the last cluster of a chain. Any entry
/// whose low 28 bits are >= 0x0FFFFFF8 ends a chain (fatgen103 Section 4); this
/// is the value we write.
pub const FAT32_EOC: u32 = 0x0fff_ffff;

/// Only the low 28 bits of a FAT32 entry are significant; the high 4 are reserved.
/// This equals FAT32_EOC numerically by coincidence; the two mean different things
/// (a significant-bits mask vs. a writable end-of-chain value) and stay separate.
const FAT32_ENTRY_MASK: u32 = 0x0fff_ffff;

/// True if a FAT32 entry (low 28 bits) marks the last cluster of a chain.
pub fn fat32_is_eoc(entry: u32) -> bool {
    (entry & FAT32_ENTRY_MASK) >= 0x0fff_fff8
}

/// An in-memory FAT32 file allocation table: one 32-bit entry per cluster,
/// indexed by cluster number. Clusters 0 and 1 are reserved; a data cluster's
/// entry is the next cluster in its chain, FAT32_EOC for the last one, or 0 when
/// free. Only the low 28 bits are significant.
pub struct Fat32Table {
    entries: Vec<u32>,
}

impl Fat32Table {
    /// A fresh FAT for `geo`, sized to clusters 0..(count_of_clusters + 2). FAT[0]
    /// holds the media byte (0xF8) with the upper bits set, FAT[1] is EOC, and
    /// every data cluster starts free (0).
    pub fn new(geo: &Fat32Geometry) -> Self {
        let mut entries = vec![0u32; geo.count_of_clusters as usize + 2];
        entries[0] = 0x0fff_fff8; // BPB_Media 0xF8 in the low 8 bits, rest ones
        entries[1] = FAT32_EOC;
        Self { entries }
    }

    /// Set cluster `cluster`'s entry to a next-cluster link or FAT32_EOC. Only the
    /// low 28 bits are stored; the reserved high 4 bits are preserved (fatgen103
    /// requires implementations to keep them across a modify). Panics if `cluster`
    /// is out of range (>= count_of_clusters + 2).
    pub fn set(&mut self, cluster: u32, value: u32) {
        let e = &mut self.entries[cluster as usize];
        *e = (*e & !FAT32_ENTRY_MASK) | (value & FAT32_ENTRY_MASK);
    }

    /// The low 28 bits of cluster `cluster`'s entry. Panics if `cluster` is out of
    /// range (>= count_of_clusters + 2).
    pub fn get(&self, cluster: u32) -> u32 {
        self.entries[cluster as usize] & FAT32_ENTRY_MASK
    }

    /// Serialize one FAT to `geo.fat_size_sectors * 512` bytes: little-endian
    /// 32-bit entries, zero-padded past the last entry. The volume holds
    /// `geo.num_fats` identical copies.
    pub fn to_bytes(&self, geo: &Fat32Geometry) -> Vec<u8> {
        let mut bytes = vec![0u8; geo.fat_size_sectors as usize * 512];
        for (i, &e) in self.entries.iter().enumerate() {
            // The FAT region is sized to hold every entry; guard regardless.
            if let Some(slot) = bytes.get_mut(i * 4..i * 4 + 4) {
                slot.copy_from_slice(&e.to_le_bytes());
            }
        }
        bytes
    }
}

/// Directory-entry attribute bit for a subdirectory (fatgen103 Section 6.2).
pub const FAT_ATTR_DIRECTORY: u8 = 0x10;

/// Build one 32-byte FAT32 directory entry. The starting cluster splits across
/// DIR_FstClusHI (offset 20) and DIR_FstClusLO (offset 26), the FAT32 difference
/// from FAT12/16 which keep the whole cluster at offset 26. `name83` is the
/// 11-byte 8.3 name field (offset 0). fatgen103 Section 6.
pub fn fat32_dir_entry(
    name83: &[u8; 11],
    attr: u8,
    cluster: u32,
    write_time: u16,
    write_date: u16,
    size: u32,
) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(name83);
    e[11] = attr;
    // e[12..20]: DIR_NTRes, creation time/date, and last-access date stay zero.
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes()); // DIR_FstClusHI
    e[22..24].copy_from_slice(&write_time.to_le_bytes()); // DIR_WrtTime
    e[24..26].copy_from_slice(&write_date.to_le_bytes()); // DIR_WrtDate
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes()); // DIR_FstClusLO
    e[28..32].copy_from_slice(&size.to_le_bytes()); // DIR_FileSize
    e
}

/// The "." and ".." entries that begin every FAT32 subdirectory (the root has
/// none). "." points at the directory's own first cluster; ".." points at the
/// parent's first cluster, or 0 when the parent is the root (fatgen103 Section
/// 6.5). Returns the two 32-byte entries back to back.
pub fn fat32_dot_entries(self_cluster: u32, parent_cluster: u32) -> [u8; 64] {
    // The dot entries carry a zero write time/date, matching the fat12 convention.
    // fatgen103 6.5 suggests the containing directory's timestamp, but DOS reads a
    // zero timestamp without complaint and no caller threads one through yet.
    let mut dot = [b' '; 11];
    dot[0] = b'.';
    let mut dotdot = [b' '; 11];
    dotdot[0] = b'.';
    dotdot[1] = b'.';
    let mut out = [0u8; 64];
    out[0..32].copy_from_slice(&fat32_dir_entry(
        &dot,
        FAT_ATTR_DIRECTORY,
        self_cluster,
        0,
        0,
        0,
    ));
    out[32..64].copy_from_slice(&fat32_dir_entry(
        &dotdot,
        FAT_ATTR_DIRECTORY,
        parent_cluster,
        0,
        0,
        0,
    ));
    out
}

#[cfg(test)]
#[path = "fat32_test.rs"]
mod tests;
