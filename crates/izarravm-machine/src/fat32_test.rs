// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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

#[test]
fn new_table_has_reserved_entries_and_free_clusters() {
    let geo = fat32_geometry(64 * 1024 * 1024).unwrap();
    let fat = Fat32Table::new(&geo);
    assert_eq!(fat.get(0), 0x0fff_fff8, "FAT[0] = media 0xF8 + EOC bits");
    assert_eq!(fat.get(1), FAT32_EOC, "FAT[1] = EOC");
    assert_eq!(fat.get(2), 0, "the first data cluster starts free");
    assert_eq!(
        fat.get(geo.count_of_clusters + 1),
        0,
        "the last data cluster starts free"
    );
}

#[test]
fn set_links_a_chain_and_keeps_only_the_low_28_bits() {
    let geo = fat32_geometry(64 * 1024 * 1024).unwrap();
    let mut fat = Fat32Table::new(&geo);
    // A 3-cluster chain 2 -> 3 -> 4 -> EOC.
    fat.set(2, 3);
    fat.set(3, 4);
    fat.set(4, FAT32_EOC);
    assert_eq!(fat.get(2), 3);
    assert_eq!(fat.get(3), 4);
    assert!(fat32_is_eoc(fat.get(4)), "cluster 4 ends the chain");
    // The reserved high 4 bits are dropped from the written value.
    fat.set(5, 0xf000_0007);
    assert_eq!(fat.get(5), 7, "only the low 28 bits are stored");
}

#[test]
fn to_bytes_is_the_fat_size_and_little_endian() {
    let geo = fat32_geometry(64 * 1024 * 1024).unwrap();
    let mut fat = Fat32Table::new(&geo);
    fat.set(2, FAT32_EOC); // a one-cluster root chain
    let bytes = fat.to_bytes(&geo);
    assert_eq!(bytes.len(), geo.fat_size_sectors as usize * 512);
    let entry = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    assert_eq!(entry(0), 0x0fff_fff8, "FAT[0]");
    assert_eq!(entry(4), FAT32_EOC, "FAT[1]");
    assert_eq!(entry(8), FAT32_EOC, "cluster 2 = EOC");
    // Past the last entry, the FAT region is zero-padded.
    let last = (geo.count_of_clusters as usize + 2) * 4;
    assert!(bytes[last..].iter().all(|&b| b == 0), "padding is zero");
}

#[test]
fn is_eoc_recognizes_the_end_markers() {
    assert!(fat32_is_eoc(0x0fff_ffff));
    assert!(
        fat32_is_eoc(0x0fff_fff8),
        "0x0FFFFFF8 is the low end of EOC"
    );
    assert!(!fat32_is_eoc(0x0fff_fff7), "one below EOC is a link");
    assert!(!fat32_is_eoc(2), "a normal next-cluster link is not EOC");
    assert!(
        fat32_is_eoc(0xffff_ffff),
        "the reserved high bits are ignored"
    );
}

#[test]
fn last_cluster_entry_survives_serialization() {
    // The highest data cluster's entry must land inside the FAT region (the
    // size invariant) and round-trip through to_bytes without the padding
    // guard clipping it.
    let geo = fat32_geometry(64 * 1024 * 1024).unwrap();
    let last = geo.count_of_clusters + 1;
    let mut fat = Fat32Table::new(&geo);
    fat.set(last, FAT32_EOC);
    let bytes = fat.to_bytes(&geo);
    let off = last as usize * 4;
    assert!(
        off + 4 <= bytes.len(),
        "the last entry is inside the FAT region"
    );
    assert_eq!(
        u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]),
        FAT32_EOC,
        "the last cluster round-trips through to_bytes"
    );
}

fn dir_cluster(e: &[u8], hi: usize, lo: usize) -> u32 {
    (u32::from(u16::from_le_bytes([e[hi], e[hi + 1]])) << 16)
        | u32::from(u16::from_le_bytes([e[lo], e[lo + 1]]))
}

#[test]
fn dir_entry_splits_the_cluster_across_hi_and_lo() {
    let name = *b"FILE    TXT";
    let e = fat32_dir_entry(&name, 0x20, 0x0123_4567, 0xbeef, 0xcafe, 42);
    assert_eq!(&e[0..11], b"FILE    TXT", "name field");
    assert_eq!(e[11], 0x20, "attribute");
    assert_eq!(u16::from_le_bytes([e[20], e[21]]), 0x0123, "FstClusHI");
    assert_eq!(u16::from_le_bytes([e[22], e[23]]), 0xbeef, "WrtTime");
    assert_eq!(u16::from_le_bytes([e[24], e[25]]), 0xcafe, "WrtDate");
    assert_eq!(u16::from_le_bytes([e[26], e[27]]), 0x4567, "FstClusLO");
    assert_eq!(
        dir_cluster(&e, 20, 26),
        0x0123_4567,
        "the cluster reassembles"
    );
    assert_eq!(u32::from_le_bytes([e[28], e[29], e[30], e[31]]), 42, "size");
    assert!(
        e[12..20].iter().all(|&b| b == 0),
        "DIR_NTRes / creation / last-access stay zero"
    );
}

#[test]
fn dir_entry_small_cluster_leaves_fstclushi_zero() {
    // The common early-volume case: a sub-64K cluster sits entirely in
    // FstClusLO with FstClusHI zero (guards the >>16 split direction).
    let e = fat32_dir_entry(b"DATA    BIN", 0x20, 2, 0, 0, 0);
    assert_eq!(
        &e[20..22],
        &[0, 0],
        "FstClusHI is zero for a sub-64K cluster"
    );
    assert_eq!(
        u16::from_le_bytes([e[26], e[27]]),
        2,
        "FstClusLO carries it"
    );
}

#[test]
fn dot_entries_point_at_self_and_parent() {
    let entries = fat32_dot_entries(5, 2);
    assert_eq!(entries[0], b'.');
    assert!(
        entries[1..11].iter().all(|&b| b == b' '),
        ". is dot + spaces"
    );
    assert_eq!(entries[11], FAT_ATTR_DIRECTORY);
    assert_eq!(
        dir_cluster(&entries[0..32], 20, 26),
        5,
        ". points at itself"
    );

    assert_eq!(entries[32], b'.');
    assert_eq!(entries[33], b'.');
    assert!(
        entries[34..43].iter().all(|&b| b == b' '),
        ".. is dotdot + spaces"
    );
    assert_eq!(entries[43], FAT_ATTR_DIRECTORY);
    assert_eq!(
        dir_cluster(&entries[32..64], 20, 26),
        2,
        ".. points at parent"
    );
}

#[test]
fn dotdot_is_zero_when_the_parent_is_the_root() {
    // fatgen103: a top-level subdirectory's ".." cluster is 0, not the root's 2.
    let entries = fat32_dot_entries(7, 0);
    assert_eq!(dir_cluster(&entries[32..64], 20, 26), 0);
}
