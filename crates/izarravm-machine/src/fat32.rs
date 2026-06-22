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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_for_1_gib() {
        let g = fat32_geometry(1024 * 1024 * 1024).unwrap();
        assert_eq!(g.bytes_per_sector, 512);
        assert_eq!(g.sectors_per_cluster, 8);
        assert_eq!(g.reserved_sectors, 32);
        assert_eq!(g.num_fats, 2);
        assert_eq!(g.total_sectors, 2_097_152);
        assert_eq!(g.fat_size_sectors, 2046);
        assert_eq!(g.root_cluster, 2);
        assert_eq!(g.fsinfo_sector, 1);
        assert_eq!(g.backup_boot_sector, 6);
        assert_eq!(g.count_of_clusters, 261_628);
        assert_eq!(g.first_data_sector, 4124);
    }

    #[test]
    fn geometry_for_64_mib_uses_single_sector_clusters() {
        let g = fat32_geometry(64 * 1024 * 1024).unwrap();
        assert_eq!(g.sectors_per_cluster, 1);
        assert_eq!(g.fat_size_sectors, 1016);
        assert_eq!(g.count_of_clusters, 129_008);
        assert_eq!(g.first_data_sector, 2064);
    }

    #[test]
    fn too_small_for_fat32_is_none() {
        // 16 MiB is well below the 32.5 MB FAT32 floor.
        assert!(fat32_geometry(16 * 1024 * 1024).is_none());
        // Exactly the table boundary (66600 sectors) is still too small.
        assert!(fat32_geometry(66_600 * 512).is_none());
    }

    #[test]
    fn just_above_the_floor_meets_the_cluster_minimum() {
        // One sector past the table floor must still be a valid FAT32, i.e. at
        // least 65525 clusters, which is exactly why the table cuts over there.
        let g = fat32_geometry(66_601 * 512).unwrap();
        assert_eq!(g.sectors_per_cluster, 1);
        assert!(
            g.count_of_clusters >= MIN_FAT32_CLUSTERS,
            "got {} clusters",
            g.count_of_clusters
        );
    }

    #[test]
    fn larger_volumes_scale_the_cluster_size() {
        assert_eq!(
            fat32_geometry(20u64 * 1024 * 1024 * 1024)
                .unwrap()
                .sectors_per_cluster,
            32,
            "20 GB -> 16 KiB clusters"
        );
        assert_eq!(
            fat32_geometry(40u64 * 1024 * 1024 * 1024)
                .unwrap()
                .sectors_per_cluster,
            64,
            "40 GB -> 32 KiB clusters"
        );
    }

    #[test]
    fn data_region_is_consistent_with_the_cluster_count() {
        // CountofClusters * SecPerClus data sectors must fit between the first
        // data sector and the end of the volume, the fatgen103 invariant.
        let g = fat32_geometry(2u64 * 1024 * 1024 * 1024).unwrap();
        let data = g.total_sectors - g.first_data_sector;
        assert_eq!(data / u32::from(g.sectors_per_cluster), g.count_of_clusters);
        assert_eq!(
            g.first_data_sector,
            u32::from(g.reserved_sectors) + u32::from(g.num_fats) * g.fat_size_sectors
        );
    }

    fn le16(s: &[u8; 512], at: usize) -> u16 {
        u16::from_le_bytes([s[at], s[at + 1]])
    }
    fn le32(s: &[u8; 512], at: usize) -> u32 {
        u32::from_le_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]])
    }

    #[test]
    fn boot_sector_has_the_fat32_bpb() {
        let geo = fat32_geometry(1024 * 1024 * 1024).unwrap();
        let s = fat32_boot_sector(&geo, 0x1234_5678);
        assert_eq!(s[0], 0xeb, "jmp opcode");
        assert_eq!(s[2], 0x90, "nop after jmp");
        assert_eq!(&s[3..11], b"MSWIN4.1");
        assert_eq!(le16(&s, 11), 512, "bytes per sector");
        assert_eq!(s[13], geo.sectors_per_cluster, "sectors per cluster");
        assert_eq!(le16(&s, 14), 32, "reserved sectors");
        assert_eq!(s[16], 2, "num FATs");
        assert_eq!(le16(&s, 17), 0, "RootEntCnt is 0 on FAT32");
        assert_eq!(le16(&s, 19), 0, "TotSec16 is 0 on FAT32");
        assert_eq!(s[21], 0xf8, "fixed-disk media descriptor");
        assert_eq!(le16(&s, 22), 0, "FATSz16 is 0 on FAT32");
        assert_eq!(le32(&s, 32), geo.total_sectors, "TotSec32");
        assert_eq!(le32(&s, 36), geo.fat_size_sectors, "BPB_FATSz32");
        assert_eq!(le32(&s, 44), 2, "BPB_RootClus");
        assert_eq!(le16(&s, 48), 1, "BPB_FSInfo");
        assert_eq!(le16(&s, 50), 6, "BPB_BkBootSec");
        assert_eq!(s[64], 0x80, "BS_DrvNum");
        assert_eq!(s[66], 0x29, "BS_BootSig");
        assert_eq!(le32(&s, 67), 0x1234_5678, "BS_VolID");
        assert_eq!(&s[82..90], b"FAT32   ", "BS_FilSysType");
        assert_eq!(s[510], 0x55, "signature lo");
        assert_eq!(s[511], 0xaa, "signature hi");
        // Fields that must read as zero on FAT32, plus the reserved and boot-code
        // regions, so a stray nonzero byte in the BPB cannot slip through.
        assert_eq!(le32(&s, 28), 0, "HiddSec");
        assert_eq!(le16(&s, 40), 0, "BPB_ExtFlags");
        assert_eq!(le16(&s, 42), 0, "BPB_FSVer");
        assert_eq!(&s[71..82], b"NO NAME    ", "BS_VolLab");
        assert!(s[52..64].iter().all(|&b| b == 0), "BPB_Reserved is zero");
        assert!(
            s[90..510].iter().all(|&b| b == 0),
            "boot-code region is zero"
        );
    }

    #[test]
    fn fsinfo_sector_has_the_signatures_and_counts() {
        let s = fat32_fsinfo_sector(261_000, 3);
        assert_eq!(le32(&s, 0), 0x4161_5252, "FSI_LeadSig");
        assert_eq!(le32(&s, 484), 0x6141_7272, "FSI_StrucSig");
        assert_eq!(le32(&s, 488), 261_000, "FSI_Free_Count");
        assert_eq!(le32(&s, 492), 3, "FSI_Nxt_Free");
        assert_eq!(le32(&s, 508), 0xaa55_0000, "FSI_TrailSig");
        assert_eq!(s[510], 0x55, "trail sig carries the 0x55AA at 510/511");
        assert_eq!(s[511], 0xaa);
        assert!(
            s[4..484].iter().all(|&b| b == 0),
            "the reserved gap is zero"
        );
    }

    #[test]
    fn fsinfo_unknown_sentinel_round_trips() {
        let s = fat32_fsinfo_sector(0xFFFF_FFFF, 0xFFFF_FFFF);
        assert_eq!(le32(&s, 488), 0xFFFF_FFFF, "free count unknown");
        assert_eq!(le32(&s, 492), 0xFFFF_FFFF, "next free unknown");
        // The signatures stay present alongside the sentinel counts.
        assert_eq!(le32(&s, 0), 0x4161_5252);
        assert_eq!(le32(&s, 508), 0xaa55_0000);
    }

    #[test]
    fn boot_sector_round_trips_through_the_geometry() {
        // A reader recomputing the cluster count from the written BPB must get the
        // same number the geometry function produced.
        let geo = fat32_geometry(64 * 1024 * 1024).unwrap();
        let s = fat32_boot_sector(&geo, 0);
        let total = le32(&s, 32);
        let fatsz = le32(&s, 36);
        let data = total - (u32::from(le16(&s, 14)) + u32::from(s[16]) * fatsz);
        assert_eq!(data / u32::from(s[13]), geo.count_of_clusters);
    }
}
