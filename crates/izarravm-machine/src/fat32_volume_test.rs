// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

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
    let dotdot_cluster = (u32::from(u16::from_le_bytes([sub_entries[1][20], sub_entries[1][21]]))
        << 16)
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

#[test]
fn multi_sector_cluster_round_trips() {
    let dir = temp_dir("bigcluster");
    // A 512 MB volume uses 8 sectors per cluster (4 KiB), so a payload that
    // crosses a cluster exercises the sector-within-cluster math and a
    // multi-cluster file chain at once.
    let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(dir.join("big.bin"), &payload).unwrap();
    let vol = build_fat32(&dir, 512 * 1024 * 1024, 1).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    assert!(vol.geometry().sectors_per_cluster > 1, "spc > 1 for 512 MB");

    let root = read_dir_entries(&vol, vol.geometry().root_cluster);
    let (cluster, size) = find_entry(&root, b"BIG     BIN").expect("BIG.BIN in root");
    assert_eq!(size as usize, payload.len());
    assert_eq!(read_file(&vol, cluster, size), payload);
}

#[test]
fn directory_spanning_multiple_clusters_is_complete() {
    let dir = temp_dir("bigdir");
    // On the 64 MB volume a cluster is one sector (16 entries). 30 root
    // entries overflow a single cluster, forcing a 2-cluster directory chain.
    for i in 0..30 {
        std::fs::write(dir.join(format!("F{i:02}.TXT")), format!("file {i}")).unwrap();
    }
    let vol = build_fat32(&dir, TEST_BYTES, 1).unwrap();
    std::fs::remove_dir_all(&dir).ok();

    let root = read_dir_entries(&vol, vol.geometry().root_cluster);
    for i in 0..30 {
        let name = format!("F{i:02}     TXT");
        let name11: [u8; 11] = name.as_bytes().try_into().unwrap();
        let (cluster, size) = find_entry(&root, &name11).unwrap_or_else(|| {
            panic!("F{i:02}.TXT survived the multi-cluster directory");
        });
        assert_eq!(
            read_file(&vol, cluster, size),
            format!("file {i}").into_bytes()
        );
    }
}
