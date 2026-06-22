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
}
