// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::fs;

fn scratch(name: &str) -> std::path::PathBuf {
    // A unique temp dir; the test cleans it up at the end.
    let p = std::env::temp_dir().join(format!("katea_tree_{}_{name}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

/// Stamp a file into a directory exactly as the guest would: allocate `nclu`
/// contiguous clusters from `first`, write the FAT chain, write the data, and
/// append the 8.3 directory entry to directory `dir_cluster` (single-cluster dirs
/// here). Returns the next free cluster. All writes go through the overlay.
#[cfg(test)]
fn stamp_file(
    vol: &mut KateaTreeVolume,
    dir_cluster: u32,
    name: &str,
    attr: u8,
    first: u32,
    data: &[u8],
) -> u32 {
    let spc = u32::from(vol.geo.spc);
    let cluster_bytes = spc as usize * SECTOR;
    // At least one cluster, even for an empty file or a fresh directory.
    let nclu = data.len().div_ceil(cluster_bytes).max(1) as u32;
    // FAT chain c -> c+1 ... -> EOC.
    for i in 0..nclu {
        let c = first + i;
        let v = if i == nclu - 1 {
            crate::fat32::FAT32_EOC
        } else {
            c + 1
        };
        let byte = c as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sec = vol.read_sector(lba);
        let off = byte % SECTOR;
        sec[off..off + 4].copy_from_slice(&(v & 0x0FFF_FFFF).to_le_bytes());
        vol.write_sector(lba, &sec);
    }
    // Data clusters.
    for (i, chunk) in data.chunks(cluster_bytes).enumerate() {
        let c = first + i as u32;
        let base = vol.cluster_to_lba(c);
        for (s, sec_bytes) in chunk.chunks(SECTOR).enumerate() {
            let mut sec = [0u8; SECTOR];
            sec[..sec_bytes.len()].copy_from_slice(sec_bytes);
            vol.write_sector(base + s as u32, &sec);
        }
    }
    // Directory entry appended to the directory's first cluster (sector 0). Read
    // the current (overlay-or-tree) directory, find the first free 32-byte slot,
    // splice the entry, write it back.
    let dir_lba = vol.cluster_to_lba(dir_cluster);
    let mut dsec = vol.read_sector(dir_lba);
    let mut name11 = [b' '; 11];
    let (b, x) = name.split_once('.').unwrap_or((name, ""));
    name11[..b.len()].copy_from_slice(b.as_bytes());
    name11[8..8 + x.len()].copy_from_slice(x.as_bytes());
    let entry = crate::fat32::fat32_dir_entry(&name11, attr, first, 0, 0, data.len() as u32);
    let slot = (0..16)
        .map(|i| i * 32)
        .find(|&o| dsec[o] == 0x00 || dsec[o] == 0xE5)
        .expect("a free dir slot in sector 0");
    dsec[slot..slot + 32].copy_from_slice(&entry);
    vol.write_sector(dir_lba, &dsec);
    first + nclu
}

/// Rewrite a one-cluster file in place and update its existing directory entry.
/// This models successive guest write commands without creating duplicate names.
#[cfg(test)]
fn rewrite_single_cluster_file(
    vol: &mut KateaTreeVolume,
    dir_cluster: u32,
    name: &str,
    first: u32,
    data: &[u8],
) {
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    assert!(data.len() <= cluster_bytes);
    for (sector, bytes) in data.chunks(SECTOR).enumerate() {
        let mut out = [0u8; SECTOR];
        out[..bytes.len()].copy_from_slice(bytes);
        vol.write_sector(vol.cluster_to_lba(first) + sector as u32, &out);
    }

    let mut name83 = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    name83[..base.len()].copy_from_slice(base.as_bytes());
    name83[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    let dir_lba = vol.cluster_to_lba(dir_cluster);
    let mut directory = vol.read_sector(dir_lba);
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| directory[offset..offset + 11] == name83)
        .expect("existing file entry");
    directory[slot + 28..slot + 32].copy_from_slice(&(data.len() as u32).to_le_bytes());
    vol.write_sector(dir_lba, &directory);
}

#[cfg(test)]
fn fresh_vol(tag: &str) -> (KateaTreeVolume, std::path::PathBuf) {
    let root = scratch(tag);
    let sys = vec![
        ("KERNEL.SYS".to_string(), vec![0xEBu8; 100]),
        ("COMMAND.COM".to_string(), vec![0u8; 50]),
    ];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();
    (vol, root)
}

#[test]
fn glide_fallback_stays_on_path_behind_a_game_local_ovl() {
    let root = scratch("glide_precedence");
    std::fs::write(root.join("GLIDE2X.OVL"), b"game local").unwrap();
    let tree = build_tree(
        &root,
        &[("GLIDE2X.OVL".to_string(), b"global fallback".to_vec())],
    );

    let local = tree
        .root
        .files
        .iter()
        .find(|file| &file.name == b"GLIDE2X OVL")
        .expect("game-local OVL remains in the current directory");
    assert!(matches!(local.source, FileSource::HostFile { .. }));

    let dos = tree
        .root
        .subdirs
        .iter()
        .find(|dir| &dir.name == b"DOS        ")
        .expect("synthetic C:\\DOS exists");
    let fallback = dos
        .dir
        .files
        .iter()
        .find(|file| &file.name == b"GLIDE2X OVL")
        .expect("global OVL is available through PATH");
    assert!(matches!(
        &fallback.source,
        FileSource::InMemory(bytes) if bytes == b"global fallback"
    ));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn reconcile_creates_a_new_file_in_the_root() {
    let (mut vol, root) = fresh_vol("rec_create");
    let free = vol.next_free;
    stamp_file(&mut vol, 2, "NEW.TXT", 0x20, free, b"created\r\n");
    vol.reconcile();
    let got = std::fs::read(root.join("NEW.TXT")).expect("NEW.TXT materialized");
    assert_eq!(got, b"created\r\n");
    // fresh_vol overlays COMMAND.COM, which lands in the synthetic C:\DOS folder.
    // That folder is InMemory-only and must never be materialized on the host.
    assert!(
        !root.join("DOS").exists(),
        "the synthetic C:\\DOS folder must not become a real host directory"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_overwrites_an_existing_host_file() {
    let root = scratch("rec_over");
    std::fs::write(root.join("OLD.TXT"), b"before!!").unwrap(); // 8 bytes
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();
    // The existing OLD.TXT occupies tree clusters; the guest rewrites it in place.
    let old_fc = vol
        .tree()
        .root
        .files
        .iter()
        .find(|f| &f.name == b"OLD     TXT")
        .unwrap()
        .first_cluster;
    // Overwrite same length (8 bytes) but different content: must still be written.
    stamp_file(&mut vol, 2, "OLD.TXT", 0x20, old_fc, b"AFTER!!!");
    vol.reconcile();
    assert_eq!(std::fs::read(root.join("OLD.TXT")).unwrap(), b"AFTER!!!");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_grows_a_file() {
    let (mut vol, root) = fresh_vol("rec_grow");
    let free = vol.next_free;
    let _next = stamp_file(&mut vol, 2, "GROW.TXT", 0x20, free, b"line1\r\n");
    vol.reconcile();
    assert_eq!(std::fs::read(root.join("GROW.TXT")).unwrap(), b"line1\r\n");
    // Grow: re-stamp the same name at the same first cluster with more data.
    stamp_file(&mut vol, 2, "GROW.TXT", 0x20, free, b"line1\r\nline2\r\n");
    vol.reconcile();
    assert_eq!(
        std::fs::read(root.join("GROW.TXT")).unwrap(),
        b"line1\r\nline2\r\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn after_write_defers_a_growing_file_until_the_final_reconcile() {
    let (mut vol, root) = fresh_vol("rec_grow_deferred");
    let first = vol.next_free + 1000;
    let initial = vec![0x11; 32];
    stamp_file(&mut vol, 2, "GROW.BIN", 0x20, first, &initial);
    vol.reconcile_after_write();

    assert_eq!(std::fs::read(root.join("GROW.BIN")).unwrap(), initial);
    let first_gathers = vol.gathers();
    let first_gathered_bytes = vol.gathered_bytes();
    let first_writes = vol.atomic_writes();
    let first_write_bytes = vol.atomic_write_bytes();
    assert_eq!(first_gathers, 1, "the first shape is mirrored immediately");
    assert_eq!(first_writes, 1, "the first shape is written immediately");

    let mut final_payload = Vec::new();
    for step in 1..=6u8 {
        final_payload = vec![step; 32 + usize::from(step) * 64];
        rewrite_single_cluster_file(&mut vol, 2, "GROW.BIN", first, &final_payload);
        vol.reconcile_after_write();

        // Real DOS traffic interleaves data with metadata and unrelated commands.
        // None of those AfterWrite passes proves that GROW.BIN is finished.
        let fsinfo_lba = vol.geo.part_start + u32::from(FSINFO_SECTOR);
        let fsinfo = vol.read_sector(fsinfo_lba);
        vol.write_sector(fsinfo_lba, &fsinfo);
        vol.reconcile_after_write();
        let unrelated_lba = vol.cluster_to_lba(first + 5000 + u32::from(step));
        vol.write_sector(unrelated_lba, &[step; SECTOR]);
        vol.reconcile_after_write();
    }

    assert_eq!(
        vol.gathers(),
        first_gathers,
        "growth must not re-gather prefixes"
    );
    assert_eq!(
        vol.gathered_bytes(),
        first_gathered_bytes,
        "growth must add no gathered bytes"
    );
    assert_eq!(
        vol.atomic_writes(),
        first_writes,
        "growth must not rewrite prefixes"
    );
    assert_eq!(
        vol.atomic_write_bytes(),
        first_write_bytes,
        "growth must add no materialized bytes"
    );
    assert_eq!(
        std::fs::read(root.join("GROW.BIN")).unwrap(),
        initial,
        "the host keeps the last completed shape until flush"
    );

    vol.reconcile();
    assert_eq!(std::fs::read(root.join("GROW.BIN")).unwrap(), final_payload);
    assert_eq!(vol.gathers(), first_gathers + 1);
    assert_eq!(
        vol.gathered_bytes(),
        first_gathered_bytes + final_payload.len() as u64
    );
    assert_eq!(vol.atomic_writes(), first_writes + 1);
    assert_eq!(
        vol.atomic_write_bytes(),
        first_write_bytes + final_payload.len() as u64
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_materializes_a_multi_cluster_file() {
    // ~1200 bytes at 512-byte clusters spans 3 clusters (the last partially
    // full), so this exercises the multi-cluster chain gather, the
    // `capacity = clusters * cluster_bytes` math, and the `data.truncate(size)`
    // that drops the final cluster's slack. A position-derived pattern makes a
    // wrong offset or a mis-truncation obvious.
    let (mut vol, root) = fresh_vol("rec_multiclu");
    let free = vol.next_free;
    let payload: Vec<u8> = (0..1200u32).map(|i| (i % 251) as u8).collect();
    stamp_file(&mut vol, 2, "BIG.DAT", 0x20, free, &payload);
    vol.reconcile();
    assert_eq!(std::fs::read(root.join("BIG.DAT")).unwrap(), payload);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_holds_a_chain_that_references_an_out_of_range_cluster() {
    // A guest-crafted FAT chain `free -> bogus -> EOC`, where `bogus` is past
    // the last valid data cluster, must be HELD (no host file, no panic) — never
    // gathered, which would compute an out-of-range LBA (a u32 overflow in debug
    // for large cluster sizes, garbage in release). Conservative-by-construction.
    let (mut vol, root) = fresh_vol("rec_oob");
    let free = vol.next_free;
    let bogus = vol.geo.count_of_clusters + 100; // beyond the last valid cluster
    let set_fat = |vol: &mut KateaTreeVolume, c: u32, v: u32| {
        let byte = c as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sec = vol.read_sector(lba);
        let off = byte % SECTOR;
        sec[off..off + 4].copy_from_slice(&(v & 0x0FFF_FFFF).to_le_bytes());
        vol.write_sector(lba, &sec);
    };
    // free -> bogus -> EOC, with data in `free`'s cluster and a dir entry whose
    // size (600 > one cluster) forces the chain to follow the bogus link.
    set_fat(&mut vol, free, bogus);
    set_fat(&mut vol, bogus, crate::fat32::FAT32_EOC);
    let base = vol.cluster_to_lba(free);
    vol.write_sector(base, &[0x42u8; SECTOR]);
    let dir_lba = vol.cluster_to_lba(2);
    let mut dsec = vol.read_sector(dir_lba);
    let entry = crate::fat32::fat32_dir_entry(
        &{
            let mut n = [b' '; 11];
            n[..3].copy_from_slice(b"OOB");
            n[8..11].copy_from_slice(b"BIN");
            n
        },
        0x20,
        free,
        0,
        0,
        600,
    );
    let slot = (0..16).map(|i| i * 32).find(|&o| dsec[o] == 0).unwrap();
    dsec[slot..slot + 32].copy_from_slice(&entry);
    vol.write_sector(dir_lba, &dsec);

    vol.reconcile(); // must not panic
    assert!(
        !root.join("OOB.BIN").exists(),
        "an out-of-range chain must be held, not materialized"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_makes_a_subdir_and_a_file_inside_it() {
    let (mut vol, root) = fresh_vol("rec_mkdir");
    // MKDIR SUB: a directory entry in the root pointing at a fresh cluster.
    let sub_fc = vol.next_free;
    stamp_file(&mut vol, 2, "SUB", 0x10, sub_fc, &[0u8; 0]); // dir, 1 cluster
    // A file inside SUB (directory cluster = sub_fc).
    let file_fc = sub_fc + 1;
    stamp_file(&mut vol, sub_fc, "DEEP.TXT", 0x20, file_fc, b"deep\r\n");
    vol.reconcile();
    assert!(root.join("SUB").is_dir(), "SUB created on host");
    assert_eq!(
        std::fs::read(root.join("SUB").join("DEEP.TXT")).unwrap(),
        b"deep\r\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_holds_an_incomplete_chain_and_skips_system_files() {
    let (mut vol, root) = fresh_vol("rec_hold");
    let free = vol.next_free;
    // Stamp a directory entry claiming size 600 (2 clusters) but only chain one
    // cluster (single-cluster EOC chain), so clusters*cb < size -> hold.
    let byte = free as usize * 4;
    let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let mut sec = vol.read_sector(lba);
    let off = byte % SECTOR;
    sec[off..off + 4].copy_from_slice(&crate::fat32::FAT32_EOC.to_le_bytes());
    vol.write_sector(lba, &sec);
    let base = vol.cluster_to_lba(free);
    vol.write_sector(base, &[0x41u8; SECTOR]);
    let dir_lba = vol.cluster_to_lba(2);
    let mut dsec = vol.read_sector(dir_lba);
    let entry = crate::fat32::fat32_dir_entry(
        &{
            let mut n = [b' '; 11];
            n[..4].copy_from_slice(b"HOLD");
            n[8..11].copy_from_slice(b"BIN");
            n
        },
        0x20,
        free,
        0,
        0,
        600, // claims 600 bytes but only 1 cluster (512) is chained
    );
    let slot = (0..16).map(|i| i * 32).find(|&o| dsec[o] == 0).unwrap();
    dsec[slot..slot + 32].copy_from_slice(&entry);
    vol.write_sector(dir_lba, &dsec);

    vol.reconcile();
    assert!(
        !root.join("HOLD.BIN").exists(),
        "incomplete file is held, not written"
    );
    // A system file name is never materialized even if it appears written.
    assert!(
        !root.join("KERNEL.SYS").exists(),
        "system file never materialized"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn walks_a_host_folder_into_a_tree_metadata_only_skipping_non_files() {
    let root = scratch("walk");
    fs::write(root.join("hello.txt"), b"hi").unwrap();
    fs::create_dir_all(root.join("GAMES/HELLO")).unwrap();
    fs::write(root.join("GAMES/HELLO/HELLO.COM"), vec![0u8; 600]).unwrap();

    // In-memory "system" files overlaid as mount does: KERNEL.SYS stays at the
    // root, while executables and drivers move into the synthetic C:\DOS folder.
    let sys = vec![
        ("KERNEL.SYS".to_string(), vec![0xEBu8; 70]),
        ("COMMAND.COM".to_string(), vec![0u8; 50]),
        ("TOKACD.SYS".to_string(), vec![0u8; 51]),
        ("IZCDEX.COM".to_string(), vec![0u8; 52]),
    ];
    let tree = build_tree(&root, &sys);

    // Root: KERNEL.SYS + hello.txt, plus the DOS and GAMES subdirs.
    assert_eq!(tree.root.files.len(), 2, "KERNEL.SYS + hello.txt");
    assert_eq!(&tree.root.files[0].name, b"KERNEL  SYS");
    assert_eq!(tree.root.subdirs.len(), 2, "DOS + GAMES");
    // The DOS binaries live in the synthetic folder, not the root.
    let dos = tree
        .root
        .subdirs
        .iter()
        .find(|s| &s.name == b"DOS        ")
        .expect("a synthetic DOS subdir");
    assert_eq!(dos.dir.files.len(), 3);
    assert_eq!(&dos.dir.files[0].name, b"COMMAND COM");
    assert_eq!(&dos.dir.files[1].name, b"TOKACD  SYS");
    assert_eq!(&dos.dir.files[2].name, b"IZCDEX  COM");
    assert!(
        !tree.root.files.iter().any(|f| &f.name == b"COMMAND COM"),
        "COMMAND.COM is not left at the root"
    );

    // GAMES -> HELLO -> HELLO.COM, len read from metadata (not contents).
    let games = tree
        .root
        .subdirs
        .iter()
        .find(|s| &s.name == b"GAMES      ")
        .expect("the host GAMES subdir");
    assert_eq!(games.dir.subdirs.len(), 1);
    let hello = &games.dir.subdirs[0].dir;
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

    let vol =
        KateaTreeVolume::new(&mbr, &vbr, &root, &sys).expect("small folder fits a FAT32 volume");

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

    // A folder whose demand stays enormous at every cluster size (here a
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
    let geo1g =
        fat32_geometry_for(|cb| (1u64 << 30) / u64::from(cb)).expect("a 1 GB folder fits FAT32");
    let two_gib_sectors = (2u64 << 30) / SECTOR as u64; // ~4.2M sectors
    assert!(
        u64::from(geo1g.total_sectors) < 8 * two_gib_sectors,
        "1 GB of files must not balloon to an ~80 GB disk (got {} sectors)",
        geo1g.total_sectors
    );
    assert_eq!(sectors_per_cluster(geo1g.part_sectors), geo1g.spc);

    // A tiny demand floors at MIN_DATA_CLUSTERS and reproduces the exact,
    // boot-tested geometry: spc=1, fatsz=741, count_of_clusters=94742.
    let small = fat32_geometry_for(|_cb| 10).expect("a tiny demand fits a FAT32 volume");
    assert_eq!(
        small.spc, 1,
        "small folder stays on the 1-sector cluster band"
    );
    assert_eq!(small.fatsz, 741, "kernel-formula FAT size");
    assert_eq!(small.count_of_clusters, 94_742, "data-cluster count");
    assert_eq!(sectors_per_cluster(small.part_sectors), small.spc);
}

#[test]
fn root_child_dotdot_points_at_cluster_zero() {
    // Per fatgen103 6.5, a directory whose parent is the root must encode
    // its `..` FstClus as 0, not the root's actual cluster (2).
    let root = scratch("dotdot");
    std::fs::create_dir_all(root.join("SUB")).unwrap();
    std::fs::write(root.join("SUB/A.TXT"), b"hi").unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 10])];
    let mut tree = build_tree(&root, &sys);
    allocate(&mut tree).expect("small folder fits a FAT32 volume");

    let sub = &tree.root.subdirs[0].dir;
    // `parent_first_cluster` still records the real parent (root = 2) for the
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

#[test]
fn new_seeds_dir_paths_mirrored_and_system_names() {
    let root = scratch("seed");
    std::fs::create_dir_all(root.join("SAVES")).unwrap();
    std::fs::write(root.join("SAVES").join("OLD.TXT"), b"before").unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    // Root cluster (2) maps to the mounted folder; SAVES maps to its host subdir.
    assert_eq!(vol.dir_paths.get(&2), Some(&root));
    let saves_fc = vol.tree().root.subdirs[0].dir.first_cluster;
    assert_eq!(vol.dir_paths.get(&saves_fc), Some(&root.join("SAVES")));

    // The SAVES subdir is recorded in mirrored under the root as a directory, so
    // disappearance and rename detection can see it.
    let mut saves = [b' '; 11];
    saves[..5].copy_from_slice(b"SAVES");
    let sub_entry = vol
        .mirrored
        .get(&(2, saves))
        .expect("SAVES mirrored under root");
    assert!(sub_entry.is_dir, "SAVES is a directory entry");
    assert_eq!(sub_entry.host_path, root.join("SAVES"));
    assert_eq!(sub_entry.first_cluster, saves_fc);

    // The existing host file is recorded in mirrored with the correct host_path.
    let mut old = [b' '; 11];
    old[..3].copy_from_slice(b"OLD");
    old[8..11].copy_from_slice(b"TXT");
    assert_eq!(
        vol.mirrored.get(&(saves_fc, old)).map(|m| &m.host_path),
        Some(&root.join("SAVES").join("OLD.TXT"))
    );

    // KERNEL.SYS is a system name (never materialized).
    let mut kern = [b' '; 11];
    kern[..6].copy_from_slice(b"KERNEL");
    kern[8..11].copy_from_slice(b"SYS");
    assert!(vol.system_names.contains(&kern));

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn fat_entry_reads_overlay_then_tree() {
    let root = scratch("fatentry");
    std::fs::write(root.join("A.TXT"), vec![0u8; 600]).unwrap(); // 2 clusters
    let sys = vec![("KERNEL.SYS".to_string(), vec![0u8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    // The tree's FAT marks cluster 2 (root) as EOC.
    assert_eq!(vol.fat_entry(2), 0x0FFF_FFFF);

    // Write a FAT sector into the overlay setting FAT[some_free] = EOC; fat_entry
    // now reflects the guest write.
    let free = vol.next_free; // first never-allocated cluster
    let byte = free as usize * 4;
    let fat_sector_rel = u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let lba = vol.geo.part_start + fat_sector_rel;
    let mut sec = vol.read_sector(lba);
    let off = byte % SECTOR;
    sec[off..off + 4].copy_from_slice(&0x0FFF_FFFFu32.to_le_bytes());
    vol.write_sector(lba, &sec);
    assert_eq!(
        vol.fat_entry(free),
        0x0FFF_FFFF,
        "overlay FAT write is visible"
    );

    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn overlay_shadows_the_tree_on_read_and_persists_writes() {
    let root = scratch("overlay");
    std::fs::write(root.join("A.TXT"), b"hello").unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    // A sector that has never been written reads the tree's value (the MBR at 0).
    let tree_mbr = vol.read_sector(0);
    assert_eq!(tree_mbr[0x1FE], 0x55, "tree MBR signature");

    // Writing an arbitrary sector shadows the tree on the next read.
    let mut patch = [0u8; 512];
    patch[0] = 0xAB;
    patch[511] = 0xCD;
    vol.write_sector(0, &patch);
    let after = vol.read_sector(0);
    assert_eq!(after[0], 0xAB, "overlay shadows the tree MBR");
    assert_eq!(after[511], 0xCD);

    // A second write to the same LBA wins (latest value).
    patch[0] = 0x12;
    vol.write_sector(0, &patch);
    assert_eq!(vol.read_sector(0)[0], 0x12, "latest write wins");

    // An unwritten data sector still reads the tree (zeros here = free space).
    let free = vol.read_sector(2048 + vol.geo.first_data_sector + 9000);
    assert_eq!(
        free, [0u8; 512],
        "unwritten sector falls through to the tree"
    );

    std::fs::remove_dir_all(&root).ok();
}

/// Free a cluster chain in the overlay FAT (set each entry to 0), the way DOS
/// does on delete. Follows `first -> EOC` via the current (overlay) FAT.
#[cfg(test)]
fn free_chain(vol: &mut KateaTreeVolume, first: u32) {
    let mut c = first;
    for _ in 0..(vol.geo.count_of_clusters + 2) {
        if c < 2 {
            break;
        }
        let next = vol.fat_entry(c);
        let byte = c as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sec = vol.read_sector(lba);
        let off = byte % SECTOR;
        sec[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
        vol.write_sector(lba, &sec);
        if !(2..0x0FFF_FFF8).contains(&next) {
            break;
        }
        c = next;
    }
}

/// Mark a directory entry deleted (first byte 0xE5) in `dir_cluster`'s sector 0.
#[cfg(test)]
fn delete_entry(vol: &mut KateaTreeVolume, dir_cluster: u32, name: &str) {
    let dir_lba = vol.cluster_to_lba(dir_cluster);
    let mut dsec = vol.read_sector(dir_lba);
    let mut n = [b' '; 11];
    let (b, x) = name.split_once('.').unwrap_or((name, ""));
    n[..b.len()].copy_from_slice(b.as_bytes());
    n[8..8 + x.len()].copy_from_slice(x.as_bytes());
    for slot in (0..16).map(|i| i * 32) {
        if dsec[slot..slot + 11] == n {
            dsec[slot] = 0xE5;
            break;
        }
    }
    vol.write_sector(dir_lba, &dsec);
}

#[test]
fn reconcile_deletes_a_host_file_when_the_guest_deletes_it() {
    let (mut vol, root) = fresh_vol("rec_del");
    let free = vol.next_free;
    stamp_file(&mut vol, 2, "GONE.TXT", 0x20, free, b"bye\r\n");
    vol.reconcile();
    assert!(root.join("GONE.TXT").exists(), "created first");
    delete_entry(&mut vol, 2, "GONE.TXT");
    free_chain(&mut vol, free);
    vol.reconcile();
    assert!(
        !root.join("GONE.TXT").exists(),
        "host file removed on delete"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_deletes_a_pre_existing_host_file() {
    let root = scratch("rec_del_pre");
    std::fs::write(root.join("OLD.TXT"), b"i was here first").unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();
    let old_fc = vol
        .tree()
        .root
        .files
        .iter()
        .find(|f| &f.name == b"OLD     TXT")
        .unwrap()
        .first_cluster;
    delete_entry(&mut vol, 2, "OLD.TXT");
    free_chain(&mut vol, old_fc);
    vol.reconcile();
    assert!(
        !root.join("OLD.TXT").exists(),
        "pre-existing host file removed"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_holds_a_delete_whose_chain_is_still_intact() {
    let (mut vol, root) = fresh_vol("rec_del_hold");
    let free = vol.next_free;
    stamp_file(&mut vol, 2, "KEEP.TXT", 0x20, free, b"safe\r\n");
    vol.reconcile();
    delete_entry(&mut vol, 2, "KEEP.TXT"); // 0xE5 the entry...
    // ...but do NOT free the chain.
    vol.reconcile();
    assert!(
        root.join("KEEP.TXT").exists(),
        "an intact-chain disappearance must be held, not deleted"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Rename a directory entry in place (DOS `ren`): same slot, same cluster, new
/// name. Finds `old` in `dir_cluster`'s sector 0 and overwrites its 11 name bytes.
#[cfg(test)]
fn rename_entry(vol: &mut KateaTreeVolume, dir_cluster: u32, old: &str, new: &str) {
    let dir_lba = vol.cluster_to_lba(dir_cluster);
    let mut dsec = vol.read_sector(dir_lba);
    let n83 = |s: &str| {
        let mut n = [b' '; 11];
        let (b, x) = s.split_once('.').unwrap_or((s, ""));
        n[..b.len()].copy_from_slice(b.as_bytes());
        n[8..8 + x.len()].copy_from_slice(x.as_bytes());
        n
    };
    let (o, m) = (n83(old), n83(new));
    let mut found = false;
    for slot in (0..16).map(|i| i * 32) {
        if dsec[slot..slot + 11] == o {
            dsec[slot..slot + 11].copy_from_slice(&m);
            found = true;
            break;
        }
    }
    assert!(found, "rename_entry: {old} not found in dir sector 0");
    vol.write_sector(dir_lba, &dsec);
}

/// Append a directory entry (no data/FAT writes) to `dir_cluster`'s sector 0,
/// modeling a move/link of an existing chain into a new directory.
#[cfg(test)]
fn stamp_file_entry_only(
    vol: &mut KateaTreeVolume,
    dir_cluster: u32,
    name: &str,
    attr: u8,
    first: u32,
    size: u32,
) {
    let dir_lba = vol.cluster_to_lba(dir_cluster);
    let mut dsec = vol.read_sector(dir_lba);
    let mut n = [b' '; 11];
    let (b, x) = name.split_once('.').unwrap_or((name, ""));
    n[..b.len()].copy_from_slice(b.as_bytes());
    n[8..8 + x.len()].copy_from_slice(x.as_bytes());
    let entry = crate::fat32::fat32_dir_entry(&n, attr, first, 0, 0, size);
    let slot = (0..16)
        .map(|i| i * 32)
        .find(|&o| dsec[o] == 0x00 || dsec[o] == 0xE5)
        .expect("a free dir slot");
    dsec[slot..slot + 32].copy_from_slice(&entry);
    vol.write_sector(dir_lba, &dsec);
}

#[test]
fn reconcile_renames_a_host_file_in_place() {
    let (mut vol, root) = fresh_vol("rec_ren");
    let free = vol.next_free;
    stamp_file(&mut vol, 2, "OLD.TXT", 0x20, free, b"keepme\r\n");
    vol.reconcile();
    assert!(root.join("OLD.TXT").exists());
    rename_entry(&mut vol, 2, "OLD.TXT", "NEW.TXT");
    vol.reconcile();
    assert!(!root.join("OLD.TXT").exists(), "old name gone");
    assert_eq!(std::fs::read(root.join("NEW.TXT")).unwrap(), b"keepme\r\n");
    std::fs::remove_dir_all(&root).ok();
}

/// The first cluster with a free (0) FAT entry at/after the tree's `next_free`,
/// so tests can allocate non-colliding clusters as they stamp into the overlay.
#[cfg(test)]
fn next_free_for_test(vol: &KateaTreeVolume) -> u32 {
    let mut c = vol.next_free;
    while vol.fat_entry(c) != 0 {
        c += 1;
        assert!(
            c < vol.geo.count_of_clusters + 2,
            "no free cluster in the test volume"
        );
    }
    c
}

/// Stamp an (empty) subdir: a directory entry in the parent + a one-cluster dir
/// data area with `.` and `..`. Returns the subdir's first cluster.
#[cfg(test)]
fn make_subdir(vol: &mut KateaTreeVolume, parent: u32, name: &str) -> u32 {
    let fc = next_free_for_test(vol);
    // FAT: one cluster, EOC.
    let byte = fc as usize * 4;
    let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let mut sec = vol.read_sector(lba);
    let off = byte % SECTOR;
    sec[off..off + 4].copy_from_slice(&crate::fat32::FAT32_EOC.to_le_bytes());
    vol.write_sector(lba, &sec);
    // `.` and `..` in the dir's first sector.
    let dot = crate::fat32::fat32_dir_entry(b".          ", 0x10, fc, 0, 0, 0);
    let dotdot = crate::fat32::fat32_dir_entry(b"..         ", 0x10, parent, 0, 0, 0);
    let dlba = vol.cluster_to_lba(fc);
    let mut dsec = [0u8; SECTOR];
    dsec[0..32].copy_from_slice(&dot);
    dsec[32..64].copy_from_slice(&dotdot);
    vol.write_sector(dlba, &dsec);
    // The subdir entry in the parent.
    stamp_file_entry_only(vol, parent, name, 0x10, fc, 0);
    fc
}

#[test]
fn reconcile_rmdirs_an_empty_host_subdir() {
    let (mut vol, root) = fresh_vol("rec_rmdir");
    let sub_fc = make_subdir(&mut vol, 2, "DEAD");
    vol.reconcile();
    assert!(root.join("DEAD").is_dir(), "subdir created");
    // RMDIR: 0xE5 the parent entry + free the dir's chain.
    delete_entry(&mut vol, 2, "DEAD");
    free_chain(&mut vol, sub_fc);
    vol.reconcile();
    assert!(!root.join("DEAD").exists(), "empty host subdir removed");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_renames_a_host_subdir() {
    let (mut vol, root) = fresh_vol("rec_dirren");
    let fc = make_subdir(&mut vol, 2, "OLDDIR");
    vol.reconcile();
    assert!(root.join("OLDDIR").is_dir());
    rename_entry(&mut vol, 2, "OLDDIR", "NEWDIR");
    vol.reconcile();
    assert!(!root.join("OLDDIR").exists(), "old dir name gone");
    assert!(root.join("NEWDIR").is_dir(), "dir renamed on host");

    // A file later created inside the (same-cluster) renamed dir must land under
    // the NEW host path — proving dir_paths was updated to NEWDIR on the rename.
    let file_fc = next_free_for_test(&vol);
    stamp_file(&mut vol, fc, "INSIDE.TXT", 0x20, file_fc, b"in\r\n");
    vol.reconcile();
    assert_eq!(
        std::fs::read(root.join("NEWDIR").join("INSIDE.TXT")).unwrap(),
        b"in\r\n",
        "a file in the renamed dir resolves to the new host path"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_holds_rmdir_of_a_nonempty_host_dir() {
    // The dir entry disappeared + chain freed, but the host dir still has a file
    // (we haven't deleted it). remove_dir must fail safely -> host dir held.
    let (mut vol, root) = fresh_vol("rec_rmdir_ne");
    let sub_fc = make_subdir(&mut vol, 2, "FULL");
    let file_fc = next_free_for_test(&vol);
    stamp_file(&mut vol, sub_fc, "IN.TXT", 0x20, file_fc, b"data\r\n");
    vol.reconcile();
    assert!(root.join("FULL").join("IN.TXT").exists());
    delete_entry(&mut vol, 2, "FULL");
    free_chain(&mut vol, sub_fc);
    vol.reconcile();
    assert!(
        root.join("FULL").is_dir(),
        "non-empty host dir is NOT removed"
    );
    assert!(
        root.join("FULL").join("IN.TXT").exists(),
        "the child file survives the held rmdir"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn reconcile_moves_a_host_file_into_a_subdir() {
    // Built by hand (not `fresh_vol`) because SUB must exist on the host BEFORE
    // mount, so the tree walk registers its cluster in `dir_paths` — the move's
    // destination dir must be known for the rename to resolve.
    let root = scratch("rec_move");
    std::fs::create_dir_all(root.join("SUB")).unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();
    let sub_fc = vol.tree().root.subdirs[0].dir.first_cluster;
    let free = vol.next_free;
    stamp_file(&mut vol, 2, "M.TXT", 0x20, free, b"moved\r\n");
    vol.reconcile();
    assert!(root.join("M.TXT").exists());
    // Move M.TXT from root into SUB: 0xE5 in root, fresh entry in SUB, same cluster.
    delete_entry(&mut vol, 2, "M.TXT");
    stamp_file_entry_only(&mut vol, sub_fc, "M.TXT", 0x20, free, 6);
    vol.reconcile();
    assert!(!root.join("M.TXT").exists(), "gone from root");
    assert_eq!(
        std::fs::read(root.join("SUB").join("M.TXT")).unwrap(),
        b"moved\r\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Give `vol` a tiny write cache, so anything it writes from here on is evicted to
/// the spill almost immediately. Must run before the first write: it replaces the
/// store wholesale.
#[cfg(test)]
fn shrink_cache(vol: &mut KateaTreeVolume, capacity: usize) {
    vol.store = crate::katea_store::SectorStore::with_capacity(capacity);
}

/// The guest's view must not depend on whether a sector's payload is still in RAM.
/// Reads consult the store first, evicted or not, and fall through to the computed
/// base view only for sectors the guest never wrote.
#[test]
fn read_sector_is_identical_once_every_write_has_been_evicted() {
    let (mut vol, root) = fresh_vol("evict_view");
    shrink_cache(&mut vol, 2);

    let base_mbr = vol.read_sector(0);
    let free = vol.next_free;
    let data_lba = vol.cluster_to_lba(free);
    let fat_lba = vol.geo.part_start + u32::from(RESERVED_SECTORS);

    // A mix of the regions that have no host-file home at all: the MBR shadow, a
    // FAT sector, and a data sector in free space.
    let mut want = std::collections::HashMap::new();
    for (lba, seed) in [(0u32, 0xABu8), (fat_lba, 0x11), (data_lba, 0x22)] {
        let mut s = [seed; SECTOR];
        s[7] = 0x5A;
        vol.write_sector(lba, &s);
        want.insert(lba, s);
    }
    // Push far more through the cache than it can hold, so the three above are
    // certainly spilled.
    for i in 0..200u32 {
        vol.write_sector(data_lba + 100 + i, &[i as u8; SECTOR]);
    }
    // Rewrite one of them after it was evicted: the new bytes must win over the
    // spilled ones.
    let mut rewritten = [0xCC; SECTOR];
    rewritten[1] = 0x99;
    vol.write_sector(fat_lba, &rewritten);
    want.insert(fat_lba, rewritten);

    for (lba, expect) in &want {
        assert_eq!(
            &vol.read_sector(*lba),
            expect,
            "sector {lba} after eviction"
        );
    }
    for i in 0..200u32 {
        assert_eq!(
            vol.read_sector(data_lba + 100 + i),
            [i as u8; SECTOR],
            "streamed sector {i}"
        );
    }
    // A sector the guest never wrote still reads the computed base view.
    assert_eq!(
        vol.read_sector(1),
        [0u8; SECTOR],
        "unwritten reserved sector"
    );
    assert_ne!(base_mbr, want[&0], "the test must actually shadow the MBR");
    std::fs::remove_dir_all(&root).ok();
}

/// The `was_written` hazard: reconcile decides what to mirror by asking whether a
/// chain was touched this session. That answer must outlive the payload's
/// residency, or a spilled file silently never reaches the host folder.
#[test]
fn reconcile_materializes_a_file_whose_sectors_were_all_evicted() {
    let (mut vol, root) = fresh_vol("evict_reconcile");
    shrink_cache(&mut vol, 2);
    let free = vol.next_free;
    // 16 sectors of payload against a 2-sector cache: everything is spilled well
    // before reconcile runs.
    let payload: Vec<u8> = (0..8192u32).map(|i| (i % 251) as u8).collect();
    stamp_file(&mut vol, 2, "BIG.BIN", 0x20, free, &payload);
    vol.reconcile();
    let got = std::fs::read(root.join("BIG.BIN")).expect("BIG.BIN materialized");
    assert_eq!(
        got, payload,
        "a spilled file must materialize byte-identically"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Reconcile re-reads a file's whole body before the fingerprint can reject it, and
/// `data_written` never resets, so without a watermark every file the guest ever
/// touched is re-read on every later pass forever. The skip must fire on the pass
/// after one that ended in a materialize.
#[test]
fn reconcile_skips_re_reading_a_file_that_cannot_have_changed() {
    let (mut vol, root) = fresh_vol("gather_skip");
    // Place A's data well clear of the root directory's own sector. The watermark
    // is per 128 KiB chunk, so a file parked next to the directory (as `next_free`
    // is on a fresh volume) is re-read whenever any entry is added to that
    // directory. That over-approximation is safe, it just costs a read, and it is
    // not what this test is about.
    let free = vol.next_free + 1000;
    stamp_file(&mut vol, 2, "A.TXT", 0x20, free, b"first file\r\n");
    vol.reconcile();
    assert!(std::fs::read(root.join("A.TXT")).is_ok());
    assert!(
        vol.gathers() >= 1,
        "the first pass must read A to materialize it"
    );

    // Nothing changed at all: the second pass must not re-read A. This is also what
    // catches the success arm dropping the watermark when it rebuilds MirrorEntry.
    let before = vol.gathers();
    vol.reconcile();
    assert_eq!(
        vol.gathers(),
        before,
        "an unchanged file must not be re-read after a pass that materialized it"
    );

    // A write to an unrelated file, placed past this chunk (256 sectors, and spc is
    // 1 here) so it cannot share A's 128 KiB span, must not disturb A either.
    let b_first = free + 1000;
    stamp_file(&mut vol, 2, "B.TXT", 0x20, b_first, b"second file\r\n");
    let before = vol.gathers();
    vol.reconcile();
    assert_eq!(
        vol.gathers(),
        before + 1,
        "only B should have been read, not A as well"
    );
    assert_eq!(
        std::fs::read(root.join("B.TXT")).unwrap(),
        b"second file\r\n"
    );
    assert_eq!(
        std::fs::read(root.join("A.TXT")).unwrap(),
        b"first file\r\n",
        "A's host bytes must be untouched"
    );

    // Rewriting A must be noticed again: the skip is a shortcut, not a stop.
    stamp_file(&mut vol, 2, "A.TXT", 0x20, free, b"rewritten!!\r\n");
    vol.reconcile();
    assert_eq!(
        std::fs::read(root.join("A.TXT")).unwrap(),
        b"rewritten!!\r\n",
        "a changed file must still be re-read and rewritten"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A genuinely broken spill file (not an injected fault) must surface as read
/// errors and cost nothing on the host. Here the live scan itself cannot be read,
/// which is the one failure that cannot be scoped to a single chain: the live set
/// is what every later phase reasons against, so the whole pass is abandoned.
#[test]
fn a_failed_read_in_the_live_scan_abandons_the_whole_pass() {
    let (mut vol, root) = fresh_vol("read_fail");
    shrink_cache(&mut vol, 2);
    let free = vol.next_free;
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 97) as u8).collect();
    stamp_file(&mut vol, 2, "KEEP.BIN", 0x20, free, &payload);
    vol.reconcile();
    assert_eq!(
        std::fs::read(root.join("KEEP.BIN")).unwrap(),
        payload,
        "materialized before the spill breaks"
    );

    // Break the spill under the store: every evicted sector now fails to read.
    let spill = vol.store.spill_path().to_path_buf();
    assert!(spill.exists(), "the tiny cache must have forced a spill");
    std::fs::OpenOptions::new()
        .write(true)
        .open(&spill)
        .unwrap()
        .set_len(0)
        .unwrap();

    vol.reconcile();
    assert!(
        vol.store.read_errors() > 0,
        "the broken spill must surface as read errors"
    );
    assert!(
        root.join("KEEP.BIN").exists(),
        "a failed read must never be taken as the guest deleting the file"
    );
    assert_eq!(
        std::fs::read(root.join("KEEP.BIN")).unwrap(),
        payload,
        "a failed read must never overwrite the host file with zeros"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The skip is only sound when the gather read nothing from the base view. A file
/// whose bytes come partly from its host file has a fourth input the watermark
/// cannot see: reconcile's own `atomic_write` rewrites that very host file, so its
/// base bytes can change with no guest write anywhere. Such a file must keep being
/// re-read, exactly as it is today.
#[test]
fn the_skip_never_fires_while_a_file_still_reads_from_its_host_bytes() {
    let root = scratch("skip_base");
    // Two sectors of host content, so overwriting the first leaves the second to be
    // read from the host file itself.
    let original: Vec<u8> = (0..1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(root.join("MIXED.BIN"), &original).unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();
    let fc = vol
        .tree()
        .root
        .files
        .iter()
        .find(|f| &f.name == b"MIXED   BIN")
        .unwrap()
        .first_cluster;

    // The guest overwrites only the first sector, leaving the second to come from
    // the host file. The directory entry keeps the original 1024-byte size.
    let mut first = [0xA5u8; SECTOR];
    first[0] = 0x01;
    vol.write_sector(vol.cluster_to_lba(fc), &first);
    vol.reconcile();
    let mut expect = first.to_vec();
    expect.extend_from_slice(&original[512..]);
    assert_eq!(
        std::fs::read(root.join("MIXED.BIN")).unwrap(),
        expect,
        "the merged file is materialized from store bytes plus host bytes"
    );

    // Nothing changed, but half this file's bytes still come from the host, so the
    // skip must not fire. Its watermark inputs (size, chain, chunk seq) are all
    // identical: only `all_present` stands between this and an unsound skip.
    let before = vol.gathers();
    vol.reconcile();
    assert!(
        vol.gathers() > before,
        "a file that reads from its host bytes must not be skipped: the host file \
         can change under it without any guest write"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// A rename moves the host path, so the base view under the chain moves with it.
/// The watermark must not survive that, or a later pass could skip a gather on the
/// strength of where the file used to be.
#[test]
fn a_rename_clears_the_gather_watermark() {
    let (mut vol, root) = fresh_vol("rename_watermark");
    let free = vol.next_free + 1000;
    stamp_file(&mut vol, 2, "BEFORE.TXT", 0x20, free, b"contents\r\n");
    vol.reconcile();
    // Skip-eligible now: a second pass leaves it alone.
    let before = vol.gathers();
    vol.reconcile();
    assert_eq!(vol.gathers(), before, "the file should be skip-eligible");

    rename_entry(&mut vol, 2, "BEFORE.TXT", "AFTER.TXT");
    vol.reconcile();
    assert!(
        root.join("AFTER.TXT").exists(),
        "the rename should reach the host"
    );
    // The renamed entry is a fresh mirror entry with no watermark, so the next pass
    // must re-read it rather than trust the old one.
    let before = vol.gathers();
    vol.reconcile();
    assert!(
        vol.gathers() > before,
        "a renamed file must be re-read: its watermark described the old path"
    );
    assert_eq!(
        std::fs::read(root.join("AFTER.TXT")).unwrap(),
        b"contents\r\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Push `count` unrelated sectors through the cache, so anything written before
/// them is evicted to the spill.
#[cfg(test)]
fn flush_cache(vol: &mut KateaTreeVolume, count: u32) {
    let cold = vol.cluster_to_lba(vol.next_free + 5000);
    for i in 0..count {
        vol.write_sector(cold + i, &[0x77; SECTOR]);
    }
}

/// The dangerous case for phase 2: an entry the guest removed from its directory
/// while its chain is still allocated. Reconcile decides "deleted" by reading the
/// chain's FAT entry, so if that read fails and returns zeros, an intact chain
/// looks freed and a real host file gets removed on the strength of an I/O error.
#[test]
fn a_failed_fat_read_never_deletes_the_host_file() {
    let (mut vol, root) = fresh_vol("read_fail_delete");
    shrink_cache(&mut vol, 30);
    // Clear of the root directory, so the directory's own FAT sector is never
    // written and phase 1 can still read it from the computed base view.
    let first = vol.next_free + 1000;
    stamp_file(
        &mut vol,
        2,
        "COLD.BIN",
        0x20,
        first,
        b"precious contents\r\n",
    );
    vol.reconcile();
    assert!(root.join("COLD.BIN").exists());

    // Evict this file's FAT sector, then drop its directory entry while leaving the
    // chain allocated: exactly the shape of a guest delete, minus the freed chain.
    flush_cache(&mut vol, 40);
    delete_entry(&mut vol, 2, "COLD.BIN");
    vol.store.fail_spill_reads();
    vol.reconcile();

    assert!(
        vol.store.read_errors() > 0,
        "the FAT read for the vanished entry must have failed"
    );
    assert!(
        root.join("COLD.BIN").exists(),
        "an unreadable FAT entry must not be taken as a freed chain: the host file \
         would be deleted on the strength of an I/O failure"
    );
    assert_eq!(
        std::fs::read(root.join("COLD.BIN")).unwrap(),
        b"precious contents\r\n"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// The dangerous case for phase 3: the chain reads fine but the file's own data
/// does not. The gathered bytes would be part real and part zeros, and reconcile
/// would write that straight over a good host file.
#[test]
fn a_failed_data_read_never_overwrites_the_host_file_with_zeros() {
    let (mut vol, root) = fresh_vol("read_fail_data");
    shrink_cache(&mut vol, 30);
    let first = vol.next_free + 1000;
    let payload: Vec<u8> = (0..2048u32).map(|i| (i % 251) as u8).collect();
    stamp_file(&mut vol, 2, "DATA.BIN", 0x20, first, &payload);
    vol.reconcile();
    assert_eq!(std::fs::read(root.join("DATA.BIN")).unwrap(), payload);

    // Evict the file's data, then bring its FAT sectors and the root directory back
    // into the cache by rewriting them unchanged, so phase 1 and the chain walk both
    // still succeed while the data behind them does not. This is the whole point:
    // with the directory spilled too, phase 1 aborts the pass and the per-file
    // bracket under test is never reached. Reads must happen before arming.
    flush_cache(&mut vol, 40);
    let dir_lba = vol.cluster_to_lba(2);
    let dir_sec = vol.read_sector(dir_lba);
    vol.write_sector(dir_lba, &dir_sec);
    for c in 0..4u32 {
        let byte = (first + c) as usize * 4;
        let fat_lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let sec = vol.read_sector(fat_lba);
        vol.write_sector(fat_lba, &sec);
    }
    // Touch one data sector, so the file is not skip-eligible and reconcile really
    // does try to re-read the whole body.
    vol.write_sector(vol.cluster_to_lba(first), &[0xEE; SECTOR]);
    vol.store.fail_spill_reads();
    vol.reconcile();

    assert!(
        vol.store.read_errors() > 0,
        "the spilled tail of the file must have failed to read"
    );
    assert_eq!(
        std::fs::read(root.join("DATA.BIN")).unwrap(),
        payload,
        "a file whose data cannot be read back must be held, never materialized \
         from the zeros that a failed read returns"
    );

    // The failed final pass records this exact file state for retry. Once the spill
    // recovers, even an inline AfterWrite pass retries it immediately rather than
    // treating it as ordinary in-progress growth.
    vol.store.restore_spill_reads();
    vol.reconcile_after_write();
    let mut recovered = vec![0xEE; SECTOR];
    recovered.extend_from_slice(&payload[SECTOR..]);
    assert_eq!(
        std::fs::read(root.join("DATA.BIN")).unwrap(),
        recovered,
        "the exact failed state must be retried on the next pass"
    );
    std::fs::remove_dir_all(&root).ok();
}

/// Names in `DOS_FOLDER_BINARIES` that are NOT in the committed image: they are
/// supplied by a runner override at mount time (see `apply_overrides` in
/// `lib.rs`), so the image has nothing to compare them against.
///
/// This list is an unenforced exemption: nothing here checks that an entry
/// actually reaches an override call site. Before adding a name, trace it to
/// its real override wiring and confirm the wiring exists -- do NOT add a name
/// here just to make `dos_folder_list_matches_the_committed_image` pass. If
/// `DOS_FOLDER_BINARIES` gained a name that is genuinely missing from the
/// image (the EDIT.COM bug this guard exists to catch), the fix is to put the
/// file in the image, not to exempt it here.
///
/// Worked example, `GLIDE2X.OVL`: `crates/izarravm/src/gui_session.rs`
/// (`MachineGeneration::initialize`) reads `self.spec.glide_ovl`, wraps it as
/// `("GLIDE2X.OVL".to_string(), bytes)`, and passes it through
/// `mount_hdd_folder_with_user_overrides` (`storage.rs`) to `apply_overrides`
/// (`lib.rs`) -- a real, traceable path from a runner-supplied override to the
/// overlay. That is the bar every entry here must clear.
const OVERRIDE_SUPPLIED: &[&str] = &["GLIDE2X.OVL"];

/// Walk the committed image's `C:\DOS` directory and return its 8.3 file names.
/// Deliberately independent of `extract_system_payload`, which flattens the
/// tree and so cannot answer "which directory was this in?".
fn image_dos_folder_names() -> Vec<String> {
    let img = izarravm_firmware::tokados_hdd_img();
    let sector = |lba: u32| -> &[u8] {
        let off = lba as usize * crate::katea_volume::SECTOR;
        &img[off..off + crate::katea_volume::SECTOR]
    };
    let le16 = |s: &[u8], at: usize| u16::from_le_bytes([s[at], s[at + 1]]);
    let le32 = |s: &[u8], at: usize| u32::from_le_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]]);

    let part_start = le32(sector(0), 0x1BE + 8);
    let vbr = sector(part_start);
    let reserved = u32::from(le16(vbr, 0x0E));
    let num_fats = u32::from(vbr[0x10]);
    let fatsz = le32(vbr, 0x24);
    let root_clus = le32(vbr, 0x2C);
    let spc = u32::from(vbr[0x0D]);
    let first_data = reserved + num_fats * fatsz;
    let fat_base = part_start + reserved;

    let fat_entry = |cluster: u32| -> u32 {
        let byte_off = cluster as usize * 4;
        let fat_sector = fat_base + (byte_off / crate::katea_volume::SECTOR) as u32;
        le32(sector(fat_sector), byte_off % crate::katea_volume::SECTOR) & 0x0FFF_FFFF
    };
    let cluster_lba = |cluster: u32| part_start + first_data + (cluster - root_clus) * spc;
    // Mirrors production's `extract_system_payload::read_chain` (`katea_volume.rs`):
    // a chain that never reaches EOC within the disk's sector bound is a corrupt or
    // cyclic FAT, not a shorter file. Panicking here (rather than silently returning
    // the partial bytes collected so far) keeps that divergence visible instead of
    // letting it masquerade as a `DOS_FOLDER_BINARIES` drift.
    let read_chain = |first: u32| -> Vec<u8> {
        let max_clusters = img.len() / crate::katea_volume::SECTOR;
        let mut out = Vec::new();
        let mut c = first;
        for _ in 0..max_clusters {
            for s in 0..spc {
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

    // Find the DOS subdirectory entry in the root, then list its files.
    let mut dos_cluster = None;
    for entry in read_chain(root_clus).chunks_exact(32) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xE5 || entry[11] == 0x0F || entry[11] & 0x08 != 0 {
            continue;
        }
        if entry[11] & 0x10 != 0 && crate::katea_volume::decode_83(&entry[0..11]) == "DOS" {
            dos_cluster = Some((le16(entry, 0x14) as u32) << 16 | le16(entry, 0x1A) as u32);
        }
    }
    let dos_cluster = dos_cluster.expect("committed image has no C:\\DOS directory");

    let mut names = Vec::new();
    for entry in read_chain(dos_cluster).chunks_exact(32) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xE5 || entry[11] == 0x0F || entry[11] & 0x08 != 0 || entry[11] & 0x10 != 0 {
            continue;
        }
        names.push(crate::katea_volume::decode_83(&entry[0..11]));
    }
    names
}

/// `DOS_FOLDER_BINARIES` and the committed image's `C:\DOS` are two hand-kept
/// lists of the same thing (the image builder's `dos_files` is the other side).
/// They drifted once already -- EDIT.COM shipped in the image's C:\DOS but was
/// missing here, so the overlay left it at the root and off the PATH.
#[test]
fn dos_folder_list_matches_the_committed_image() {
    let mut in_image: Vec<String> = image_dos_folder_names();
    // HELLO.TXT is a data file filtered out of the overlay before placement.
    in_image.retain(|name| name != "HELLO.TXT");
    in_image.sort();

    let mut declared: Vec<String> = DOS_FOLDER_BINARIES
        .iter()
        .filter(|name| !OVERRIDE_SUPPLIED.contains(name))
        .map(|name| (*name).to_string())
        .collect();
    declared.sort();

    assert_eq!(
        declared, in_image,
        "DOS_FOLDER_BINARIES has drifted from the committed image's C:\\DOS"
    );
}

/// The binary search in `data_sector` must pick exactly the run the old linear
/// `find` did, for every cluster the tree covers AND for the free space past it.
/// Checked against a literal re-implementation of the linear scan rather than
/// against expected bytes, so the property is "same answer", not "some answer".
#[test]
fn run_table_binary_search_agrees_with_a_linear_scan_over_every_cluster() {
    let root = scratch("runsearch");
    // Enough entries, at enough depths, that a wrong index lands on a wrong run
    // rather than coincidentally on the right one.
    for d in 0..6 {
        let dir = root.join(format!("DIR{d}"));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..7 {
            fs::write(dir.join(format!("F{f}.BIN")), vec![(d * 7 + f) as u8; 900]).unwrap();
        }
    }
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    assert!(vol.runs.len() > 40, "want a table worth searching");
    // The sortedness and disjointness the search relies on.
    for pair in vol.runs.windows(2) {
        assert!(
            pair[0].0 <= pair[1].0,
            "runs must be sorted by first_cluster"
        );
        assert!(pair[0].1 < pair[1].0, "runs must be disjoint and non-empty");
    }

    let highest = vol.runs.iter().map(|r| r.1).max().unwrap();
    for cluster in 0..=highest + 8 {
        let linear = vol
            .runs
            .iter()
            .position(|(first, last, _)| cluster >= *first && cluster <= *last);
        let searched = vol
            .runs
            .partition_point(|(first, _, _)| *first <= cluster)
            .checked_sub(1)
            .filter(|&i| cluster <= vol.runs[i].1);
        assert_eq!(linear, searched, "disagreement at cluster {cluster}");
    }

    fs::remove_dir_all(&root).ok();
}

/// The cached host handle must serve byte-identical data to a fresh open, and
/// must actually collapse the opens: one per file, not one per 512-byte sector.
#[test]
fn cached_host_handle_serves_identical_bytes_with_one_open_per_file() {
    let root = scratch("handlecache");
    // Several clusters' worth, so one file spans many sectors.
    let big: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
    let other: Vec<u8> = (0..40_000u32).map(|i| (i % 241) as u8).collect();
    fs::write(root.join("BIG.BIN"), &big).unwrap();
    fs::write(root.join("OTHER.BIN"), &other).unwrap();
    let sys = vec![("KERNEL.SYS".to_string(), vec![0xEBu8; 100])];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    // The tree does not carry host names, so identify each file by its first
    // sector: the two payloads differ from byte 0 (modulo 251 against 241).
    let find = |want: &[u8]| {
        vol.tree()
            .root
            .files
            .iter()
            .find(|f| {
                f.source.len() == want.len() as u64
                    && vol.read_sector(vol.cluster_to_lba(f.first_cluster))[..] == want[..SECTOR]
            })
            .map(|f| f.first_cluster)
            .expect("both payloads are in the tree")
    };
    let big_first = find(&big);
    let other_first = find(&other);
    assert_ne!(big_first, other_first, "the two payloads must be distinct");

    let sectors = 40_000usize.div_ceil(SECTOR);
    let before = vol.storage_counters();

    // Read BIG.BIN straight through, then OTHER.BIN, then BIG.BIN again.
    for (first, want) in [(big_first, &big), (other_first, &other), (big_first, &big)] {
        for s in 0..sectors {
            let lba = vol.cluster_to_lba(first) + s as u32;
            let got = vol.read_sector(lba);
            let start = s * SECTOR;
            let end = (start + SECTOR).min(want.len());
            assert_eq!(&got[..end - start], &want[start..end], "sector {s} differs");
        }
    }

    let after = vol.storage_counters();
    let served = after.host_file_reads - before.host_file_reads;
    let opened = after.host_file_opens - before.host_file_opens;
    assert_eq!(served as usize, sectors * 3, "every sector still counted");
    // Three sequential whole-file passes over two distinct paths: one open each.
    assert_eq!(opened, 3, "want one open per file pass, got {opened}");
    assert!(
        served > opened * 20,
        "the cache must collapse opens: {served} sectors over {opened} opens"
    );

    fs::remove_dir_all(&root).ok();
}

/// A guest write that reconcile materializes to the host must be visible to the
/// very next read. The cached handle would otherwise keep serving the file's
/// pre-write bytes: on Windows `File::open` shares rename and delete, so the
/// stale handle stays valid and simply points at the wrong content.
#[test]
fn reconcile_invalidates_the_cached_host_handle() {
    let root = scratch("cacheinval");
    fs::write(root.join("VICTIM.BIN"), vec![0xAAu8; 600]).unwrap();
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    let sys = vec![
        ("KERNEL.SYS".to_string(), vec![0xEBu8; 100]),
        ("COMMAND.COM".to_string(), vec![0u8; 50]),
    ];
    let mut vol = KateaTreeVolume::new(&mbr, &vbr, &root, &sys).unwrap();

    let victim = vol
        .tree()
        .root
        .files
        .iter()
        .find(|f| f.source.len() == 600)
        .expect("VICTIM.BIN is in the tree")
        .first_cluster;
    let lba = vol.cluster_to_lba(victim);

    // Prime the cache on the original contents.
    assert_eq!(vol.read_sector(lba)[0], 0xAA);
    assert!(vol.host_read_cache.borrow().is_some(), "cache is primed");

    // Rewrite the host file behind Katea's back, then run the reconcile that is
    // the documented invalidation point.
    fs::write(root.join("VICTIM.BIN"), vec![0x5Cu8; 600]).unwrap();
    vol.reconcile();
    assert!(
        vol.host_read_cache.borrow().is_none(),
        "reconcile must drop the cached handle"
    );
    assert_eq!(
        vol.read_sector(lba)[0],
        0x5C,
        "the next read must see the new host bytes, not the cached handle's"
    );

    fs::remove_dir_all(&root).ok();
}
