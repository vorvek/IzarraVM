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

fn write_fat_link(vol: &mut KateaTreeVolume, cluster: u32, next: u32) {
    let byte = cluster as usize * 4;
    let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let mut sector = vol.read_sector(lba);
    let offset = byte % SECTOR;
    sector[offset..offset + 4].copy_from_slice(&(next & 0x0FFF_FFFF).to_le_bytes());
    vol.write_sector(lba, &sector);
}

fn write_cluster_bytes(vol: &mut KateaTreeVolume, first: u32, data: &[u8]) {
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    for (cluster_index, chunk) in data.chunks(cluster_bytes).enumerate() {
        for (sector_index, bytes) in chunk.chunks(SECTOR).enumerate() {
            let mut sector = [0u8; SECTOR];
            sector[..bytes.len()].copy_from_slice(bytes);
            vol.write_sector(
                vol.cluster_to_lba(first + cluster_index as u32) + sector_index as u32,
                &sector,
            );
        }
    }
}

fn update_file_size(vol: &mut KateaTreeVolume, dir_cluster: u32, name: &[u8; 11], size: u32) {
    let lba = vol.cluster_to_lba(dir_cluster);
    let mut directory = vol.read_sector(lba);
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| &directory[offset..offset + 11] == name)
        .expect("existing file entry");
    directory[slot + 28..slot + 32].copy_from_slice(&size.to_le_bytes());
    vol.write_sector(lba, &directory);
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
fn after_write_streams_a_growing_file_before_the_final_reconcile() {
    let (mut vol, root) = fresh_vol("rec_grow_live");
    let first = vol.next_free + 1000;
    let initial = vec![0x11; 32];
    stamp_file(&mut vol, 2, "GROW.BIN", 0x20, first, &initial);
    vol.reconcile_after_write();

    assert_eq!(std::fs::read(root.join("GROW.BIN")).unwrap(), initial);
    let first_gathers = vol.gathers();
    let first_gathered_bytes = vol.gathered_bytes();
    let first_writes = vol.atomic_writes();
    let first_write_bytes = vol.atomic_write_bytes();
    assert_eq!(
        first_gathers, 0,
        "the first shape must stream without a whole-file gather"
    );
    assert_eq!(
        first_writes, 0,
        "the first shape must not use an atomic whole-file rewrite"
    );

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
        std::fs::read(root.join("GROW.BIN")).unwrap(),
        final_payload,
        "every completed growth command must reach the host before flush"
    );

    assert_eq!(
        vol.gathers(),
        first_gathers,
        "growth must not re-gather prefixes"
    );
    assert_eq!(
        vol.gathered_bytes(),
        first_gathered_bytes,
        "growth must add no whole-file gathered bytes"
    );
    assert_eq!(
        vol.atomic_writes(),
        first_writes,
        "growth must not atomically rewrite prefixes"
    );
    assert_eq!(
        vol.atomic_write_bytes(),
        first_write_bytes,
        "growth must add no atomically materialized prefix bytes"
    );

    vol.reconcile();
    assert_eq!(std::fs::read(root.join("GROW.BIN")).unwrap(), final_payload);
    assert_eq!(vol.gathers(), first_gathers);
    assert_eq!(vol.gathered_bytes(), first_gathered_bytes);
    assert_eq!(vol.atomic_writes(), first_writes);
    assert_eq!(vol.atomic_write_bytes(), first_write_bytes);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn data_and_fat_before_directory_project_when_the_path_appears() {
    let (mut vol, root) = fresh_vol("data_before_directory");
    let first = vol.next_free + 1000;
    let payload = b"payload arrived before its name\r\n";

    write_cluster_bytes(&mut vol, first, payload);
    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::Deferred
    );
    write_fat_link(&mut vol, first, FAT32_EOC);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(!root.join("LATE.TXT").exists());

    stamp_file_entry_only(&mut vol, 2, "LATE.TXT", 0x20, first, payload.len() as u32);
    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::Projected
    );
    assert_eq!(std::fs::read(root.join("LATE.TXT")).unwrap(), payload);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn directory_and_data_before_fat_project_when_the_chain_becomes_valid() {
    let (mut vol, root) = fresh_vol("directory_before_fat");
    let first = vol.next_free + 1000;
    let payload = b"chain arrives last\r\n";

    stamp_file_entry_only(&mut vol, 2, "CHAIN.TXT", 0x20, first, payload.len() as u32);
    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::Deferred
    );
    write_cluster_bytes(&mut vol, first, payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(!root.join("CHAIN.TXT").exists());

    write_fat_link(&mut vol, first, FAT32_EOC);
    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::Projected
    );
    assert_eq!(std::fs::read(root.join("CHAIN.TXT")).unwrap(), payload);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn nested_create_rename_and_delete_are_live_at_command_boundaries() {
    let (mut vol, root) = fresh_vol("nested_live_lifecycle");
    let sub = make_subdir(&mut vol, 2, "SUB");
    let first = sub + 1000;
    stamp_file(&mut vol, sub, "OLD.TXT", 0x20, first, b"nested\r\n");
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        std::fs::read(root.join("SUB").join("OLD.TXT")).unwrap(),
        b"nested\r\n"
    );

    rename_entry(&mut vol, sub, "OLD.TXT", "NEW.TXT");
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(!root.join("SUB").join("OLD.TXT").exists());
    assert!(root.join("SUB").join("NEW.TXT").exists());

    delete_entry(&mut vol, sub, "NEW.TXT");
    free_chain(&mut vol, first);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(!root.join("SUB").join("NEW.TXT").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn truncate_and_fragmented_growth_reach_the_host_at_command_boundaries() {
    let (mut vol, root) = fresh_vol("truncate_fragmented_live");
    let first = vol.next_free + 1000;
    let second = first + 7;
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    let initial = vec![0x21; 1000];
    stamp_file(&mut vol, 2, "FRAG.BIN", 0x20, first, &initial);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);

    update_file_size(&mut vol, 2, b"FRAG    BIN", 73);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        std::fs::read(root.join("FRAG.BIN")).unwrap(),
        &initial[..73]
    );

    let mut payload = vec![0x31; cluster_bytes];
    payload.extend(vec![0x42; 37]);
    write_cluster_bytes(&mut vol, first, &payload[..cluster_bytes]);
    write_cluster_bytes(&mut vol, second, &payload[cluster_bytes..]);
    write_fat_link(&mut vol, first, second);
    write_fat_link(&mut vol, second, FAT32_EOC);
    update_file_size(&mut vol, 2, b"FRAG    BIN", payload.len() as u32);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(std::fs::read(root.join("FRAG.BIN")).unwrap(), payload);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
#[ignore = "host performance acceptance; run alone with --release"]
fn sequential_17_3_mib_install_projects_without_spill_at_udma2_rate() {
    const INSTALL_BYTES: usize = 18_125_725;
    const COMMAND_SECTORS: usize = 256;
    const UDMA2_BYTES_PER_SECOND: f64 = 33_300_000.0;

    let (mut vol, root) = fresh_vol("installer_17m");
    let first = vol.next_free + 1000;
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    let clusters = INSTALL_BYTES.div_ceil(cluster_bytes) as u32;
    for index in 0..clusters {
        let cluster = first + index;
        let next = if index + 1 == clusters {
            crate::fat32::FAT32_EOC
        } else {
            cluster + 1
        };
        let byte = cluster as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sector = vol.read_sector(lba);
        let offset = byte % SECTOR;
        sector[offset..offset + 4].copy_from_slice(&(next & 0x0FFF_FFFF).to_le_bytes());
        vol.write_sector(lba, &sector);
    }
    stamp_file_entry_only(&mut vol, 2, "BANANA.DAT", 0x20, first, 0);
    assert_ne!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );
    assert!(
        root.join("BANANA.DAT").exists(),
        "the file appears before data arrives"
    );

    let sectors = INSTALL_BYTES.div_ceil(SECTOR);
    for command_start in (0..sectors).step_by(COMMAND_SECTORS) {
        let command_end = (command_start + COMMAND_SECTORS).min(sectors);
        for sector_index in command_start..command_end {
            let mut sector = [0u8; SECTOR];
            let byte_start = sector_index * SECTOR;
            let valid = (INSTALL_BYTES - byte_start).min(SECTOR);
            for (index, byte) in sector[..valid].iter_mut().enumerate() {
                *byte = ((byte_start + index) % 251) as u8;
            }
            let cluster_index = sector_index / usize::from(vol.geo.spc);
            let sector_in_cluster = sector_index % usize::from(vol.geo.spc);
            let lba = vol.cluster_to_lba(first + cluster_index as u32) + sector_in_cluster as u32;
            vol.write_sector(lba, &sector);
        }
        assert_ne!(
            vol.commit_guest_write_batch(GuestWriteRoute::Int13),
            CommitGuestWriteResult::HostIoFailure
        );
    }

    let dir_lba = vol.cluster_to_lba(2);
    let mut directory = vol.read_sector(dir_lba);
    let slot = (0..16)
        .map(|index| index * 32)
        .find(|&offset| &directory[offset..offset + 11] == b"BANANA  DAT")
        .unwrap();
    directory[slot + 28..slot + 32].copy_from_slice(&(INSTALL_BYTES as u32).to_le_bytes());
    vol.write_sector(dir_lba, &directory);
    assert_ne!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );

    let counters = vol.storage_counters();
    assert_eq!(
        counters.spill_operations, 0,
        "a mapped sequential install must not spill"
    );
    assert_eq!(counters.spill_bytes, 0);
    assert_eq!(counters.pending_unmapped_sectors, 0);
    let last_sector_index = sectors - 1;
    let last_cluster_index = last_sector_index / usize::from(vol.geo.spc);
    let last_sector_in_cluster = last_sector_index % usize::from(vol.geo.spc);
    let last_lba =
        vol.cluster_to_lba(first + last_cluster_index as u32) + last_sector_in_cluster as u32;
    assert!(!vol.store.is_pending(last_lba));
    let rate =
        counters.projection_bytes as f64 / (counters.projection_wall_ns as f64 / 1_000_000_000.0);
    println!(
        "katea projection throughput: {:.1} MiB/s",
        rate / 1_048_576.0
    );
    assert!(
        rate >= UDMA2_BYTES_PER_SECOND,
        "projection rate {rate:.1} B/s is below UDMA2"
    );

    let host = std::fs::read(root.join("BANANA.DAT")).unwrap();
    assert_eq!(host.len(), INSTALL_BYTES);
    assert!(
        host.iter()
            .enumerate()
            .all(|(index, byte)| *byte == (index % 251) as u8),
        "guest and host payloads differ"
    );
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn host_write_failure_reports_an_error_and_retains_data_for_retry() {
    let (mut vol, root) = fresh_vol("host_write_failure");
    let first = vol.next_free + 1000;
    let payload = b"retry survives host failure\r\n";
    stamp_file(&mut vol, 2, "BLOCK.DAT", 0x20, first, payload);
    std::fs::create_dir(root.join("BLOCK.DAT")).unwrap();

    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );
    let failed = vol.storage_counters();
    assert!(failed.host_write_failures > 0);
    assert!(
        failed.overlay_pending_sectors > 0,
        "failed payload must remain retryable"
    );

    std::fs::remove_dir(root.join("BLOCK.DAT")).unwrap();
    let dir_lba = vol.cluster_to_lba(2);
    let directory = vol.read_sector(dir_lba);
    vol.write_sector(dir_lba, &directory);
    assert_ne!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );
    assert_eq!(std::fs::read(root.join("BLOCK.DAT")).unwrap(), payload);
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn ambiguous_cluster_ownership_is_safely_deferred() {
    let (mut vol, root) = fresh_vol("ambiguous_owner");
    let first = vol.next_free + 1000;
    let payload = b"one chain cannot own two files";
    stamp_file(&mut vol, 2, "FIRST.DAT", 0x20, first, payload);
    stamp_file_entry_only(&mut vol, 2, "SECOND.DAT", 0x20, first, payload.len() as u32);

    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::Deferred
    );
    assert!(!root.join("FIRST.DAT").exists());
    assert!(!root.join("SECOND.DAT").exists());
    assert!(vol.storage_counters().overlay_pending_sectors > 0);
    drop(vol);
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
    // Stamp a directory entry claiming a size that needs two clusters but only
    // chain one (single-cluster EOC chain), so clusters*cb < size -> hold. The
    // claim has to exceed ONE cluster at the derived 4 KiB cluster size, or the
    // single chained cluster would cover it and the file would be complete.
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
        6_000, // claims 6,000 bytes but only 1 cluster (4,096) is chained
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
    // Two clusters at the derived 4 KiB cluster size (see
    // `no_folder_size_derives_five_hundred_twelve_byte_clusters`).
    std::fs::write(root.join("SUB/A.TXT"), vec![0u8; 4_696]).unwrap();
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
    // A.TXT spans 2 clusters: 4,696 bytes over 4,096-byte clusters.
    assert_eq!(u32::from(geo.spc) * SECTOR as u32, 4_096);
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
    std::fs::write(root.join("A.TXT"), vec![0u8; 4_696]).unwrap(); // 2 clusters at 4 KiB
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
    // 16 directory entries per 512-byte sector, 8 sectors per 4 KiB cluster, so
    // one cluster holds 128 entries: it takes more than 128 files to span two.
    for i in 0..200 {
        std::fs::write(root.join(format!("F{i:03}.TXT")), b"x").unwrap();
    }
    let mut tree = build_tree(&root, &[]);
    allocate(&mut tree).expect("small folder fits a FAT32 volume");
    assert!(
        tree.root.cluster_count >= 2,
        "200 entries need > 1 cluster at 128 entries per cluster"
    );
    // Second sector (entries 16..32 in directory order) holds the 17th+ entries.
    let s1 = dir_sector(&tree.root, true, 1);
    // The walk sorts F000.TXT..F199.TXT and there are no subdirs/system files,
    // so the 17th directory entry (0-based index 16) is F016.TXT.
    assert_eq!(
        &s1[0..11],
        b"F016    TXT",
        "sector 1, entry 0 is the 17th file"
    );
    assert_eq!(s1[11], crate::katea_volume::ATTR_ARCHIVE, "a file entry");
    // Sector 8 is the FIRST SECTOR OF THE SECOND CLUSTER (8 sectors per 4 KiB
    // cluster), which is what this test is named for: entry 128 is F128.TXT.
    let s8 = dir_sector(&tree.root, true, 8);
    assert_eq!(
        &s8[0..11],
        b"F128    TXT",
        "sector 8 opens the second cluster with the 129th file"
    );
    // The 200th (last) file lands at index 199 -> sector 12, entry 7.
    let s12 = dir_sector(&tree.root, true, 12);
    assert_eq!(
        &s12[7 * 32..7 * 32 + 11],
        b"F199    TXT",
        "entry 199 is F199.TXT"
    );
    // Entries past the 200th are zero-padded.
    assert_eq!(s12[8 * 32], 0x00, "no entry past the last file");
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

    // A tiny demand floors at MIN_PART_SECTORS, which puts it on the 4 KiB
    // cluster band. The exact numbers are pinned because the whole point of the
    // floor is that the derived geometry is not a function of how small the
    // folder happens to be.
    let small = fat32_geometry_for(|_cb| 10).expect("a tiny demand fits a FAT32 volume");
    assert_eq!(small.spc, 8, "a small folder still gets 4 KiB clusters");
    assert_eq!(small.fatsz, 521, "kernel-formula FAT size at spc=8");
    assert_eq!(small.count_of_clusters, 66_561, "data-cluster count");
    assert!(
        small.part_sectors >= MIN_PART_SECTORS,
        "the partition floor is what forces the band: {} sectors",
        small.part_sectors
    );
    assert_eq!(sectors_per_cluster(small.part_sectors), small.spc);
}

/// The degenerate geometry this floor exists to prevent, stated as a property
/// rather than as one pinned number: NO folder, at any size, may derive
/// 512-byte clusters. At spc=1 a 44 MB game archive is an ~86,600-entry FAT
/// chain that DOS re-walks on every seek, and 86.2% of every sector a measured
/// DUKEMARK run read was a FAT sector.
///
/// NON-VACUOUS: restoring the old `MIN_DATA_CLUSTERS = 94_742` cluster floor
/// makes the first four demands derive spc=1 and fails on the very first one.
/// The large demands are here so the test cannot pass by the floor alone --
/// they clear the floor by their own size and must still never read back spc=1.
#[test]
fn no_folder_size_derives_five_hundred_twelve_byte_clusters() {
    for demand_bytes in [
        0u64,
        1 << 10,
        48 << 20,   // the duke3d_c fixture: the folder that exposed this
        200 << 20,  // still under the old 260 MB spc=1 band ceiling
        640 << 20,  // the owner's real c_drive
        4u64 << 30, // a large folder that clears the floor on its own
    ] {
        let geo = fat32_geometry_for(|cb| (demand_bytes / u64::from(cb)).max(1))
            .expect("every one of these fits a FAT32 volume");
        assert!(
            geo.spc >= 8,
            "{demand_bytes} bytes derived spc={}, a 512-byte-cluster volume",
            geo.spc
        );
        assert_eq!(
            sectors_per_cluster(geo.part_sectors),
            geo.spc,
            "the BPB stays self-consistent at {demand_bytes} bytes"
        );
    }
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
    // The walked path resolves the same two sources in the same order: an
    // overlay HIT here (the guest wrote this FAT sector) and an overlay MISS
    // for cluster 2, whose FAT sector nothing has written.
    assert_eq!(
        vol.fat_entry_via_walk(free),
        0x0FFF_FFFF,
        "the walk missed the overlay it must read"
    );
    assert_eq!(
        vol.fat_entry_via_walk(2),
        0x0FFF_FFFF,
        "the walk's base-view arm disagreed with the tree's own FAT"
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
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(root.join("M.TXT").exists());
    // Move M.TXT from root into SUB: 0xE5 in root, fresh entry in SUB, same cluster.
    delete_entry(&mut vol, 2, "M.TXT");
    stamp_file_entry_only(&mut vol, sub_fc, "M.TXT", 0x20, free, 6);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
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
    // Zero, not three. The `find` probe above already opened both files, and the
    // handle cache is an LRU deep enough to hold both, so all three passes --
    // including the return to BIG.BIN after OTHER.BIN displaced it as most
    // recent -- reuse a handle that is already open. A single-entry cache
    // reopened on every change of file, which is the shape an asset load
    // produces whenever two files interleave.
    assert_eq!(opened, 0, "want no reopen at all, got {opened}");
    assert_eq!(vol.host_read_handles(), 2, "both paths stay cached");
    assert!(
        served > 20,
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
    assert!(
        vol.host_read_handle_cached(&root.join("VICTIM.BIN")),
        "cache is primed"
    );

    // Rewrite the host file behind Katea's back, then run the reconcile that is
    // the documented invalidation point.
    fs::write(root.join("VICTIM.BIN"), vec![0x5Cu8; 600]).unwrap();
    vol.reconcile();
    assert_eq!(
        vol.host_read_handles(),
        0,
        "reconcile must drop the cached handle"
    );
    assert!(
        !vol.readahead_holds(&root.join("VICTIM.BIN")),
        "reconcile must drop the read-ahead buffer with it"
    );
    assert_eq!(
        vol.read_sector(lba)[0],
        0x5C,
        "the next read must see the new host bytes, not the cached handle's"
    );

    fs::remove_dir_all(&root).ok();
}

/// The kernel's compiled-in fallback shell must name a file the image actually
/// ships, at the path it ships it at.
///
/// This default is only reached when CONFIG.SYS supplies no `SHELL=` -- which is
/// exactly what pressing F5 at the boot prompt does. Upstream FreeDOS defaults
/// it to a bare `command.com`, i.e. the boot drive's ROOT, and that is where
/// FreeDOS puts COMMAND.COM. Toka-DOS does not: the root holds only CONFIG.SYS
/// and AUTOEXEC.BAT and every binary lives in `C:\DOS`. So the stock default
/// stranded anyone who pressed F5 at "Bad or missing Command Interpreter" --
/// breaking the one escape hatch the F5 window exists to provide, and only in
/// the situation where the user already believes something is wrong.
///
/// Asserted against the shipped image rather than the source so it survives a
/// kernel rebuild, a vendored-source refresh, or a change to where the image
/// builder places binaries -- any of which can reintroduce it.
#[test]
fn the_kernels_fallback_shell_exists_where_the_image_puts_it() {
    let kernel = image_root_file("KERNEL.SYS");
    let contains = |needle: &str| kernel.windows(needle.len()).any(|w| w == needle.as_bytes());

    assert!(
        contains(r"C:\DOS\COMMAND.COM"),
        r"the kernel's fallback shell must be the full C:\DOS path"
    );
    assert!(
        contains(r" C:\DOS /P"),
        r"the fallback tail must pass the C:\DOS directory: it is what FreeCOM
         builds COMSPEC from, so a shell that loads without it cannot reload
         its own transient part"
    );
    assert!(
        image_dos_folder_names().iter().any(|n| n == "COMMAND.COM"),
        r"the image must actually ship COMMAND.COM in C:\DOS"
    );
}

/// Read a root-directory file out of the committed image by 8.3 name.
fn image_root_file(name: &str) -> Vec<u8> {
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
    let read_chain = |first: u32, limit: usize| -> Vec<u8> {
        let mut out = Vec::new();
        let mut c = first;
        for _ in 0..img.len() / crate::katea_volume::SECTOR {
            for s in 0..spc {
                out.extend_from_slice(sector(cluster_lba(c) + s));
            }
            if out.len() >= limit {
                out.truncate(limit);
                return out;
            }
            let next = fat_entry(c);
            if next >= 0x0FFF_FFF8 {
                return out;
            }
            c = next;
        }
        panic!("katea: cluster chain from {first} exceeds the disk; corrupt FAT")
    };

    for entry in read_chain(root_clus, usize::MAX).chunks_exact(32) {
        if entry[0] == 0x00 {
            break;
        }
        if entry[0] == 0xE5 || entry[11] == 0x0F || entry[11] & 0x10 != 0 {
            continue;
        }
        if crate::katea_volume::decode_83(&entry[0..11]) == name {
            let first = (le16(entry, 0x14) as u32) << 16 | le16(entry, 0x1A) as u32;
            let size = le32(entry, 0x1C) as usize;
            return read_chain(first, size);
        }
    }
    panic!("committed image has no root file named {name}");
}

/// The FAT / directory / free-space region census is a DEFAULT-OFF instrument
/// and must prove both legs: silent when unarmed, counting when armed. This
/// repo has paid for a default-on instrument taxing the path it only meant to
/// observe, and each of these increments is a read-modify-write of the whole
/// counter block on the per-sector read path.
///
/// NON-VACUOUS: deleting either `if self.region_census` guard makes the unarmed
/// volume count and fails the first block; hard-wiring the field to `false`
/// fails the second.
#[test]
fn the_region_census_is_silent_until_armed_and_counts_after() {
    fn touch_every_region(vol: &KateaTreeVolume) {
        // One FAT sector, the root directory's first sector, and a free cluster
        // well past anything allocated -- the three arms that count.
        vol.read_sector(vol.geo.part_start + u32::from(RESERVED_SECTORS));
        vol.read_sector(vol.cluster_to_lba(2));
        vol.read_sector(vol.geo.part_start + vol.geo.first_data_sector + 40_000);
    }

    let (mut vol, root) = fresh_vol("region_census");
    touch_every_region(&vol);
    let quiet = vol.storage_counters();
    assert_eq!(
        (quiet.fat_sector_reads, quiet.dir_or_free_sector_reads),
        (0, 0),
        "an unarmed volume must not count, even after reading every counted region"
    );
    assert!(
        quiet.sector_reads >= 3,
        "the reads really happened ({} sectors served)",
        quiet.sector_reads
    );

    vol.arm_region_census();
    touch_every_region(&vol);
    let armed = vol.storage_counters();
    assert_eq!(armed.fat_sector_reads, 1, "the FAT read counted");
    assert_eq!(
        armed.dir_or_free_sector_reads, 2,
        "the directory read and the free-space read counted"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_stale_directory_size_must_not_truncate_already_projected_bytes() {
    // DOS writes the FAT and the payload while the directory entry still carries
    // the size the file had at create time (0). Any unrelated directory write in
    // the same pass makes the reconcile stream this file against that stale size.
    let (mut vol, root) = fresh_vol("stale_dir_size_truncate");
    let first = vol.next_free + 1000;
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    let payload: Vec<u8> = (0..cluster_bytes).map(|i| (i % 251) as u8).collect();

    write_fat_link(&mut vol, first, FAT32_EOC);
    stamp_file_entry_only(&mut vol, 2, "STALE.BIN", 0x20, first, 0);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(root.join("STALE.BIN").exists(), "empty file appears first");

    write_cluster_bytes(&mut vol, first, &payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        std::fs::read(root.join("STALE.BIN")).unwrap(),
        payload,
        "the payload streamed to the host"
    );

    // An unrelated directory write: same bytes back, which is what a timestamp or
    // a sibling entry update looks like to the projection layer.
    let dir_lba = vol.cluster_to_lba(2);
    let directory = vol.read_sector(dir_lba);
    vol.write_sector(dir_lba, &directory);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        std::fs::read(root.join("STALE.BIN")).unwrap(),
        payload,
        "a stale directory size must not discard projected payload"
    );

    // Close: the guest finally publishes the real size.
    update_file_size(&mut vol, 2, b"STALE   BIN", payload.len() as u32);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    vol.reconcile();
    assert_eq!(
        std::fs::read(root.join("STALE.BIN")).unwrap(),
        payload,
        "the closed file must hold the guest payload, not zeros"
    );
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
#[cfg(windows)]
fn a_guest_file_named_for_a_win32_device_becomes_a_real_host_file() {
    // Win32 resolves CON in every directory, so a bare decode_83 would open the
    // console: the payload would go to a terminal and read back as nothing.
    let (mut vol, root) = fresh_vol("win32_device_name");
    let first = vol.next_free + 1000;
    let payload = b"not the console\r\n";
    stamp_file(&mut vol, 2, "CON", 0x20, first, payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);

    assert_eq!(
        std::fs::read(root.join("CON+")).unwrap(),
        payload,
        "the guest's CON is a real file under an escaped name"
    );

    // The rule covers the name followed by any extension: NUL.TXT is NUL.
    let second = first + 8;
    stamp_file(&mut vol, 2, "NUL.TXT", 0x20, second, payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(std::fs::read(root.join("NUL+.TXT")).unwrap(), payload);

    // An ordinary name that merely starts with a device name is untouched.
    let third = second + 8;
    stamp_file(&mut vol, 2, "CONFIG.SY_", 0x20, third, payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(std::fs::read(root.join("CONFIG.SY_")).unwrap(), payload);
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
#[cfg(windows)]
fn the_device_name_guard_maps_stems_and_extensions_case_insensitively() {
    assert_eq!(host_child_name(b"CON        "), "CON+");
    assert_eq!(host_child_name(b"con        "), "con+");
    assert_eq!(host_child_name(b"NUL     TXT"), "NUL+.TXT");
    assert_eq!(host_child_name(b"LPT1    DAT"), "LPT1+.DAT");
    assert_eq!(host_child_name(b"COM9       "), "COM9+");
    // Not reserved: a longer stem, and a device name only in the extension.
    assert_eq!(host_child_name(b"CONS       "), "CONS");
    assert_eq!(host_child_name(b"README  CON"), "README.CON");
}

// --- item 2: the unmapped-sector cluster index ------------------------------

/// Write `sectors` payload sectors starting at `first`, with no FAT chain and no
/// directory entry, so nothing can project them.
fn write_unprojectable(vol: &mut KateaTreeVolume, first: u32, sectors: usize, fill: u8) {
    for index in 0..sectors {
        let cluster_index = index / usize::from(vol.geo.spc);
        let sector_in_cluster = index % usize::from(vol.geo.spc);
        let lba = vol.cluster_to_lba(first + cluster_index as u32) + sector_in_cluster as u32;
        vol.write_sector(lba, &[fill; SECTOR]);
    }
}

#[test]
fn a_commit_examines_only_the_clusters_it_touched() {
    let (mut vol, root) = fresh_vol("unmapped_scope");
    let spc = usize::from(vol.geo.spc);
    let stale = vol.next_free + 1000;
    // Four clusters of payload that nothing can map. They stay unmapped.
    write_unprojectable(&mut vol, stale, spc * 4, 0x11);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        vol.storage_counters().pending_unmapped_sectors,
        (spc * 4) as u64
    );

    // A later, unrelated command touches one cluster elsewhere. It must not walk
    // the four stale clusters again: the whole point of indexing by cluster.
    let other = stale + 64;
    vol.reset_candidate_census();
    write_unprojectable(&mut vol, other, spc, 0x22);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        vol.candidate_lbas_examined(),
        spc as u64,
        "a commit must examine its own cluster's sectors, not every unmapped sector ever written"
    );
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn unmapped_sectors_reach_the_host_once_their_cluster_becomes_projectable() {
    let (mut vol, root) = fresh_vol("unmapped_then_mapped");
    let spc = usize::from(vol.geo.spc);
    let cluster_bytes = spc * SECTOR;
    let first = vol.next_free + 1000;
    // Payload lands before the FAT chain and the directory entry exist.
    let payload: Vec<u8> = (0..cluster_bytes * 2).map(|i| (i % 251) as u8).collect();
    write_cluster_bytes(&mut vol, first, &payload);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert_eq!(
        vol.storage_counters().pending_unmapped_sectors,
        (spc * 2) as u64,
        "payload with no owner is held, not projected"
    );

    // Now the guest publishes the chain and the entry, exactly as DOS closes a file.
    write_fat_link(&mut vol, first, first + 1);
    write_fat_link(&mut vol, first + 1, FAT32_EOC);
    stamp_file_entry_only(&mut vol, 2, "LATE.BIN", 0x20, first, payload.len() as u32);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    // The directory write forces a metadata pass, which streams the whole chain.
    assert_eq!(std::fs::read(root.join("LATE.BIN")).unwrap(), payload);
    assert_eq!(
        vol.storage_counters().pending_unmapped_sectors,
        0,
        "projected payload must leave the unmapped index"
    );
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_reused_cluster_projects_under_its_new_owner() {
    let (mut vol, root) = fresh_vol("unmapped_cluster_reuse");
    let spc = usize::from(vol.geo.spc);
    let cluster_bytes = spc * SECTOR;
    let first = vol.next_free + 1000;

    // A file is created, then deleted and its chain freed: the cluster is free.
    let stale = vec![0x41u8; cluster_bytes];
    stamp_file(&mut vol, 2, "GONE.BIN", 0x20, first, &stale);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(root.join("GONE.BIN").exists());
    delete_entry(&mut vol, 2, "GONE.BIN");
    free_chain(&mut vol, first);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    assert!(!root.join("GONE.BIN").exists());

    // A new file reuses the very same cluster. Its payload arrives first, with no
    // owner, and only then the chain and the entry.
    let fresh: Vec<u8> = (0..cluster_bytes).map(|i| (i % 97) as u8).collect();
    write_cluster_bytes(&mut vol, first, &fresh);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);
    write_fat_link(&mut vol, first, FAT32_EOC);
    stamp_file_entry_only(&mut vol, 2, "REUSE.BIN", 0x20, first, fresh.len() as u32);
    vol.commit_guest_write_batch(GuestWriteRoute::Int13);

    assert_eq!(std::fs::read(root.join("REUSE.BIN")).unwrap(), fresh);
    assert_eq!(vol.storage_counters().pending_unmapped_sectors, 0);
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

/// The shape the cluster index exists for: guest payload that lands before
/// anything can map it, command after command. Held sectors used to be swept in
/// full on every commit, so the work grew with the square of what the session
/// had written. Asserted on the sweep's own census rather than on wall, because
/// the census is exactly what the index bounds and it is the same on every host.
///
/// Measured on the author's host at a393404e (flat set) against the index, min
/// of three interleaved runs: 128 commands 102.1 ms -> 33.2 ms, and the sweep
/// inside it 58.5 ms -> 1.6 ms.
#[test]
fn held_sectors_do_not_make_a_write_run_quadratic() {
    const COMMAND_SECTORS: usize = 256;
    let mut examined = Vec::new();
    for commands in [16usize, 32, 64] {
        let (mut vol, root) = fresh_vol(&format!("unmapped_growth_{commands}"));
        let first = vol.next_free + 1000;
        let spc = usize::from(vol.geo.spc);
        vol.reset_candidate_census();
        for command in 0..commands {
            let base = first + (command * COMMAND_SECTORS / spc) as u32;
            write_unprojectable(&mut vol, base, COMMAND_SECTORS, 0x77);
            assert_ne!(
                vol.commit_guest_write_batch(GuestWriteRoute::Int13),
                CommitGuestWriteResult::HostIoFailure
            );
        }
        assert_eq!(
            vol.storage_counters().pending_unmapped_sectors,
            (commands * COMMAND_SECTORS) as u64,
            "every unprojectable sector stays held, and the index counts it exactly once"
        );
        examined.push(vol.candidate_lbas_examined());
        drop(vol);
        std::fs::remove_dir_all(&root).ok();
    }
    // Linear: each run writes its own sectors once and revisits nobody else's.
    for (index, commands) in [16usize, 32, 64].iter().enumerate() {
        assert_eq!(
            examined[index],
            (commands * COMMAND_SECTORS) as u64,
            "a run of {commands} commands examined {} candidate sectors",
            examined[index]
        );
    }
}

/// A host write that fails strands its sectors in the held index, and the next
/// commit must retry them. The cluster is projectable already -- what failed is
/// the host write, which is not a projection event -- so nothing re-arms the
/// revisit ticket unless the failure path does it itself.
///
/// Deliberately never calls `reconcile`/`flush_guest_writes`: the final full walk
/// recovers the sector either way, which is exactly why the missing per-commit
/// retry is invisible from the existing host-write-failure row.
#[test]
fn a_failed_host_write_leaves_its_sectors_retryable_on_the_next_commit() {
    let (mut vol, root) = fresh_vol("failed_write_retry");
    let cluster_bytes = usize::from(vol.geo.spc) * SECTOR;
    let first = vol.next_free + 1000;
    let payload = vec![0x61u8; cluster_bytes];
    stamp_file(&mut vol, 2, "RETRY.BIN", 0x20, first, &payload);
    // `reconcile` materializes through `atomic_write`, which drops the host
    // write handle, so the host file can be replaced below.
    vol.reconcile();
    assert_eq!(std::fs::read(root.join("RETRY.BIN")).unwrap(), payload);

    // A directory where the file was: the next open of that path fails.
    std::fs::remove_file(root.join("RETRY.BIN")).unwrap();
    std::fs::create_dir(root.join("RETRY.BIN")).unwrap();

    // One data sector inside the projected chain. No FAT write and no directory
    // write, so this commit takes the streaming path, not the metadata pass.
    let mut sector = [0x62u8; SECTOR];
    sector[0] = 0xA5;
    vol.write_sector(vol.cluster_to_lba(first), &sector);
    assert_eq!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );
    // The whole cluster is held, not just the sector this commit wrote: the
    // payload `stamp_file` laid down arrived before any projection owned it, and
    // `atomic_write` -- how `reconcile` materialized the file -- acknowledges
    // nothing, so those sectors were still held when this write joined them.
    assert_eq!(
        vol.storage_counters().pending_unmapped_sectors,
        u64::from(vol.geo.spc),
        "the sectors the host write could not place are held for a retry"
    );

    // Put the host file back, then commit nothing at all. The retry owes itself
    // to the held sector alone: this batch is empty and writes no metadata.
    std::fs::remove_dir(root.join("RETRY.BIN")).unwrap();
    std::fs::write(root.join("RETRY.BIN"), &payload).unwrap();
    assert_ne!(
        vol.commit_guest_write_batch(GuestWriteRoute::Int13),
        CommitGuestWriteResult::HostIoFailure
    );

    let host = std::fs::read(root.join("RETRY.BIN")).unwrap();
    assert_eq!(
        &host[..SECTOR],
        &sector[..],
        "the held sector reached the host"
    );
    assert_eq!(vol.storage_counters().pending_unmapped_sectors, 0);
    drop(vol);
    std::fs::remove_dir_all(&root).ok();
}

// ---------------------------------------------------------------------------
// Cross-command read-ahead, the read-handle LRU, and the projection's scaling.
// ---------------------------------------------------------------------------

/// Mount `root` and return the volume. Same boot sectors and system files as
/// `fresh_vol`, but over a folder the caller has already populated.
fn mount(root: &std::path::Path) -> KateaTreeVolume {
    let sys = vec![
        ("KERNEL.SYS".to_string(), vec![0xEBu8; 100]),
        ("COMMAND.COM".to_string(), vec![0u8; 50]),
    ];
    let img = izarravm_firmware::tokados_hdd_img();
    let mut mbr = [0u8; 512];
    mbr.copy_from_slice(&img[0..512]);
    let mut vbr = [0u8; 512];
    vbr.copy_from_slice(&img[2048 * 512..2048 * 512 + 512]);
    KateaTreeVolume::new(&mbr, &vbr, root, &sys).unwrap()
}

/// The guest cluster a mounted host file starts at, found by its host path.
fn first_cluster_of(vol: &KateaTreeVolume, path: &std::path::Path) -> u32 {
    fn walk(dir: &TreeDir, path: &std::path::Path) -> Option<u32> {
        for f in &dir.files {
            if let FileSource::HostFile { path: p, .. } = &f.source
                && p == path
            {
                return Some(f.first_cluster);
            }
        }
        dir.subdirs.iter().find_map(|s| walk(&s.dir, path))
    }
    walk(&vol.tree().root, path).expect("the file is in the tree")
}

/// Read `sectors` sectors starting at `lba` as one guest read command.
fn read_command(vol: &KateaTreeVolume, lba: u32, sectors: u32) -> Vec<u8> {
    vol.begin_read_command(lba, sectors);
    let mut out = Vec::with_capacity(sectors as usize * SECTOR);
    for s in 0..sectors {
        out.extend_from_slice(&vol.read_sector(lba + s));
    }
    vol.end_read_command();
    out
}

/// THE READ WINDOW MUST SURVIVE THE COMMAND THAT FILLED IT.
///
/// A DOS asset load is not one big command: it is a long run of separate 8-sector
/// (one 4 KiB cluster) INT 13h commands over one file. The PR 726 window is
/// discarded at every command boundary, so each of those commands paid its own
/// synchronous host `seek`+`read` -- which is what the 0.5-8 ms read tail in the
/// duke3d-486 hitch profile was made of. The read-ahead buffer is keyed by host
/// path and byte range rather than by LBA, so it can outlive the command.
///
/// The ramp is what bounds the cost: the fill starts at the command extent and
/// only doubles where the previous fill for this path ended, so the physical
/// bytes a sequential stream reads stay within a small factor of the bytes it is
/// served, however far it runs.
///
/// NON-VACUOUS: dropping the read-ahead in `end_read_command` (i.e. giving it the
/// window's lifetime) makes every command a physical read again and fails the
/// `host_read_operations` assertion.
#[test]
fn readahead_serves_later_commands_out_of_one_host_read() {
    let root = scratch("readahead_span");
    let commands = 32u32;
    let data: Vec<u8> = (0..commands * 8 * SECTOR as u32)
        .map(|i| (i % 251) as u8)
        .collect();
    let path = root.join("BIG.BIN");
    fs::write(&path, &data).unwrap();
    let mut vol = mount(&root);
    // Pin the OTHER switch, so this measures one mechanism whichever leg of
    // `IZARRAVM_HDD_COMMAND_READ_BATCH` the suite is being run under. The two are
    // independent axes: with the command batch off, the first fill collapses to
    // a single sector and the ramp has to climb the whole way from there.
    vol.command_read_batch_enabled = true;
    let lba = vol.cluster_to_lba(first_cluster_of(&vol, &path));

    let before = vol.storage_counters();
    for command in 0..commands {
        let got = read_command(&vol, lba + command * 8, 8);
        let start = command as usize * 8 * SECTOR;
        assert_eq!(
            &got[..],
            &data[start..start + 8 * SECTOR],
            "command {command} served the wrong bytes"
        );
    }
    let after = vol.storage_counters();

    let operations = after.host_read_operations - before.host_read_operations;
    assert!(
        operations <= 8,
        "{commands} commands must not cost {operations} host reads"
    );
    assert!(
        after.host_readahead_fills > before.host_readahead_fills,
        "and the saving must come from fills that ran past the command"
    );
    let hits = after.host_readahead_hits - before.host_readahead_hits;
    assert!(
        hits >= u64::from(commands * 8) - operations,
        "every sector not physically read must come from the buffer, got {hits}"
    );
    // Logical bytes served are unchanged by any of this.
    let served = after.host_bytes - before.host_bytes;
    assert_eq!(
        served,
        u64::from(commands * 8) * SECTOR as u64,
        "the guest was served exactly what it asked for"
    );
    // The ramp's bound, asserted rather than argued.
    let physical = after.host_read_bytes - before.host_read_bytes;
    assert!(
        physical <= 3 * served,
        "read {physical} physical bytes to serve {served}"
    );

    fs::remove_dir_all(&root).ok();
}

/// AMPLIFICATION MUST STAY BOUNDED WHEN TWO FILES INTERLEAVE.
///
/// The read-ahead's failure mode is a fill that is large, unearned, and thrown
/// away: a miss costs a physical read of the whole fill, so a pattern that misses
/// every time turns 512 bytes served into a fill's worth of host I/O, on the
/// emulation thread, in exactly the cold-file case this exists to help. Two files
/// read a sector at a time in turn is that pattern.
///
/// Two things hold the line. The slots are an LRU, so alternation still hits
/// rather than evicting on every read; and the fill is earned per path, so even a
/// pure miss stream can only ever read what its command asked for.
///
/// NON-VACUOUS: one slot instead of `HOST_READAHEAD_SLOTS` drops the hits to zero;
/// a flat `HOST_READAHEAD_MAX_BYTES` fill instead of the ramp reads a fill per
/// sector and blows the byte bound by two orders of magnitude.
#[test]
fn interleaved_single_sector_reads_do_not_amplify() {
    let root = scratch("readahead_interleave");
    // Comfortably larger than one maximum fill, so a flat fill would not be
    // clamped by the file size and the bound below really is about the ramp.
    let bytes = HOST_READAHEAD_MAX_BYTES as usize * 2;
    let names = ["ONE.BIN", "TWO.BIN"];
    for (i, name) in names.iter().enumerate() {
        fs::write(root.join(name), vec![0xB0u8 | i as u8; bytes]).unwrap();
    }
    let vol = mount(&root);
    let lbas: Vec<u32> = names
        .iter()
        .map(|name| vol.cluster_to_lba(first_cluster_of(&vol, &root.join(name))))
        .collect();

    let before = vol.storage_counters();
    // No `begin_read_command` at all: a bare single-sector probe, which is what a
    // reconcile gather looks like and the least the read-ahead is ever told.
    for sector in 0..64u32 {
        for (i, lba) in lbas.iter().enumerate() {
            assert_eq!(
                vol.read_sector(lba + sector)[0],
                0xB0 | i as u8,
                "file {i} sector {sector}"
            );
        }
    }
    let after = vol.storage_counters();

    let served = after.host_bytes - before.host_bytes;
    let physical = after.host_read_bytes - before.host_read_bytes;
    assert_eq!(served, 128 * SECTOR as u64, "128 sectors were asked for");
    // Three, not four: a first-touch fill that ignored the command extent and
    // took a fixed multiple of it would still slip under a looser bound. Measured
    // at 1.98x, so this leaves half again as much room as the ramp actually uses.
    // The other half of that invariant -- that a first touch reads the command
    // extent and NOTHING more -- is pinned exactly, one file over, by
    // `the_sector_cache_hits_misses_and_charges_on_a_katea_host_folder`.
    assert!(
        physical <= 3 * served,
        "alternating single-sector reads pulled {physical} physical bytes to \
         serve {served}"
    );
    assert!(
        after.host_readahead_hits > before.host_readahead_hits,
        "and the alternation must still hit: one slot per file, not one slot"
    );

    fs::remove_dir_all(&root).ok();
}

/// The kill switch has to put the read path back on the PR 726 behaviour exactly,
/// so an A/B is same-binary.
#[test]
fn the_readahead_kill_switch_restores_a_host_read_per_command() {
    let root = scratch("readahead_off");
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let path = root.join("BIG.BIN");
    fs::write(&path, &data).unwrap();
    let mut vol = mount(&root);
    vol.disarm_readahead();
    // Pin the OTHER switch, so this test measures one mechanism whichever leg of
    // `IZARRAVM_HDD_COMMAND_READ_BATCH` the suite is being run under.
    vol.command_read_batch_enabled = true;
    let lba = vol.cluster_to_lba(first_cluster_of(&vol, &path));

    let before = vol.storage_counters();
    for command in 0..3u32 {
        let got = read_command(&vol, lba + command * 8, 8);
        let start = command as usize * 8 * SECTOR;
        assert_eq!(
            &got[..],
            &data[start..start + 8 * SECTOR],
            "the bytes must not depend on the switch"
        );
    }
    let after = vol.storage_counters();

    assert_eq!(
        after.host_read_operations - before.host_read_operations,
        3,
        "disarmed: one host read per command, as before"
    );
    assert_eq!(
        after.host_readahead_hits - before.host_readahead_hits,
        0,
        "and no read-ahead at all"
    );
    // The command window is a separate mechanism and must still be doing its job:
    // 8 sectors per command, 3 physical reads, not 24.
    assert_eq!(
        after.host_read_bytes - before.host_read_bytes,
        3 * 8 * SECTOR as u64,
        "the per-command window still coalesces"
    );

    fs::remove_dir_all(&root).ok();
}

/// THE WRITE-THROUGH PATH IS ITS OWN INVALIDATION POINT.
///
/// `stream_projected_batch` -> `stream_file_overlay` writes a guest sector
/// straight into the host file and then acknowledges it out of the overlay, so
/// the next read of that sector resolves through the host file again. It runs
/// with no reconcile around it, so the blanket drop in `reconcile_mode` never
/// sees it. A read-ahead buffer filled before the write holds the file's old
/// bytes for exactly that sector.
///
/// NON-VACUOUS: deleting the `invalidate_host_reads` call from
/// `stream_file_overlay` fails the final assertion with the pre-write byte.
#[test]
fn a_write_through_drops_the_readahead_for_that_file() {
    let root = scratch("readahead_writethrough");
    let data: Vec<u8> = vec![0xA1u8; 200_000];
    let path = root.join("BIG.BIN");
    fs::write(&path, &data).unwrap();
    let mut vol = mount(&root);
    let lba = vol.cluster_to_lba(first_cluster_of(&vol, &path));

    // Prime the read-ahead over the file's opening bytes.
    assert_eq!(read_command(&vol, lba, 8)[0], 0xA1);
    assert!(
        vol.readahead_holds(&path),
        "the buffer is primed on this file"
    );

    // The guest overwrites one sector; the commit streams it through to the host
    // file and acknowledges it, so reads fall back to the host from here on.
    let sector = [0x5Cu8; SECTOR];
    vol.write_sector(lba, &sector);
    assert_eq!(
        vol.reconcile_after_write(),
        CommitGuestWriteResult::Projected,
        "the write must reach the host file"
    );
    assert!(
        !vol.store.is_pending(lba),
        "and be acknowledged out of the overlay, or this proves nothing"
    );
    assert!(
        !vol.readahead_holds(&path),
        "the write-through must drop the buffer it just invalidated"
    );

    assert_eq!(
        vol.read_sector(lba)[0],
        0x5C,
        "the next read must see the written bytes, not the buffered ones"
    );

    fs::remove_dir_all(&root).ok();
}

/// The handle cache is an LRU with a bound: a session that touches many files
/// must not hold a descriptor for each, and the most recently used must survive.
#[test]
fn the_read_handle_cache_is_a_bounded_lru() {
    let root = scratch("handle_lru");
    let count = MAX_HOST_READ_HANDLES + 3;
    let paths: Vec<std::path::PathBuf> = (0..count)
        .map(|i| {
            let path = root.join(format!("F{i:02}.BIN"));
            fs::write(&path, vec![i as u8; 4096]).unwrap();
            path
        })
        .collect();
    let vol = mount(&root);

    for (i, path) in paths.iter().enumerate() {
        let lba = vol.cluster_to_lba(first_cluster_of(&vol, path));
        assert_eq!(vol.read_sector(lba)[0], i as u8, "file {i} served wrong");
    }

    assert_eq!(
        vol.host_read_handles(),
        MAX_HOST_READ_HANDLES,
        "the cache must stay bounded"
    );
    for (i, path) in paths.iter().enumerate() {
        assert_eq!(
            vol.host_read_handle_cached(path),
            i >= count - MAX_HOST_READ_HANDLES,
            "wrong eviction verdict for file {i}"
        );
    }

    fs::remove_dir_all(&root).ok();
}

/// A ONE-FILE WRITE MUST NOT COST THE WHOLE VOLUME.
///
/// A projection pass walks the cluster chain of every live file three times over
/// (the ownership scan, the materialize scan, and the pending re-check), and each
/// step of a walk asks `fat_entry` for one cluster. `fat_entry` reads a whole FAT
/// sector, and synthesizing one costs 128 hashed set probes -- so the pass paid
/// ~128 probes per cluster ON THE VOLUME for a write that touched one file.
/// Measured on a 47 MB duke3d-486 folder: single passes of 290, 196 and 125 ms,
/// each synchronous on the emulation thread.
///
/// The memo makes the FAT synthesis proportional to the FAT REGION the pass
/// touches (one build per 128 clusters at worst) instead of to the clusters it
/// walks. This asserts that ratio directly.
///
/// NON-VACUOUS: returning `self.fat.fat_sector(within, &self.geo)` from
/// `base_fat_sector` without consulting or filling the memo makes builds exceed
/// the walked-cluster count and fails the ratio assertion by two orders of
/// magnitude.
#[test]
fn a_one_file_write_does_not_resynthesize_the_fat_per_cluster() {
    let root = scratch("projection_scaling");
    // Enough clusters that "proportional to the volume" and "proportional to the
    // write" are far apart, in few enough root entries that `stamp_file` still
    // finds a free slot in the root directory's first sector.
    for i in 0..8u32 {
        fs::write(root.join(format!("A{i:02}.BIN")), vec![i as u8; 384 * 1024]).unwrap();
    }
    let mut vol = mount(&root);
    let allocated = vol.fat.next_free() - ROOT_CLUSTER;
    assert!(
        allocated > 400,
        "want a volume worth walking, got {allocated} clusters"
    );

    // One small new file, exactly the measured case: a guest creating a file in a
    // directory it dirties. This is the write that used to project the volume.
    let first = vol.fat.next_free();
    stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "NEW.TXT",
        ATTR_ARCHIVE,
        first,
        b"hi",
    );
    vol.reset_fat_sector_builds();
    let before = vol.storage_counters();
    vol.reconcile_after_write();
    let after = vol.storage_counters();

    assert!(
        after.metadata_projection_passes > before.metadata_projection_passes,
        "this must be the metadata-projection path, or the test is vacuous"
    );
    assert_eq!(
        fs::read(root.join("NEW.TXT")).unwrap(),
        b"hi",
        "and it must actually project the file"
    );
    let builds = vol.fat_sector_builds();
    assert!(
        builds * 8 < u64::from(allocated),
        "a one-file write synthesized {builds} FAT sectors over {allocated} \
         allocated clusters: the pass is still paying per cluster"
    );

    fs::remove_dir_all(&root).ok();
}

/// Build a volume with a mixture of MOUNT-TIME chains and GUEST-WRITTEN ones,
/// so a FAT differential has both an overlay-served region and a base-view one,
/// plus a chain the guest fragmented backwards through the FAT.
fn seeded_volume(name: &str) -> (KateaTreeVolume, std::path::PathBuf) {
    let root = scratch(name);
    for i in 0..6u32 {
        fs::write(root.join(format!("H{i:02}.BIN")), vec![i as u8; 384 * 1024]).unwrap();
    }
    let mut vol = mount(&root);
    // Three guest-written files, one of them long enough to span several FAT
    // sectors (128 clusters is one FAT sector at 4 bytes an entry).
    let mut next = vol.fat.next_free();
    next = stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "G0.BIN",
        ATTR_ARCHIVE,
        next,
        &[1u8; 4096],
    );
    let long_first = next;
    next = stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "G1.BIN",
        ATTR_ARCHIVE,
        next,
        &vec![2u8; 300 * 4096],
    );
    stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "G2.BIN",
        ATTR_ARCHIVE,
        next,
        &[3u8; 8192],
    );
    // Fragment G1: point its first cluster's FAT entry backwards, at a cluster
    // in a LOWER FAT sector, so a walk crosses sector boundaries in both
    // directions and a cursor that only ever moves forward would be caught.
    let byte = long_first as usize * 4;
    let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let mut sec = vol.read_sector(lba);
    let off = byte % SECTOR;
    sec[off..off + 4].copy_from_slice(&(long_first + 200).to_le_bytes());
    vol.write_sector(lba, &sec);
    (vol, root)
}

/// THE WALKED FAT MUST ANSWER EXACTLY WHAT `fat_entry` ANSWERS.
///
/// `fat_entry_walked` reaches the base-view FAT through `ClusterIndex::fat_entry`
/// instead of through a synthesized 512-byte sector, and it hoists the overlay
/// lookup to once per FAT sector. Both are claims about COST. This is the claim
/// about VALUE: over every cluster of a volume that has an overlay-shadowed FAT
/// region, a base-view region, and entries past the end of the FAT, the two paths
/// agree.
///
/// Swept three ways -- fresh cursor per cluster, one cursor ascending, one cursor
/// descending -- because the cursor is the only state either path carries, and a
/// stale-cursor bug shows up only when consecutive calls land in different FAT
/// sectors.
///
/// NON-VACUOUS: dropping the `walk.within != Some(within)` guard (so the cursor
/// never refreshes) fails the ascending sweep on the first sector boundary.
#[test]
fn the_walked_fat_agrees_with_fat_entry_over_every_cluster() {
    let (vol, root) = seeded_volume("fat_walk_identity");
    let last = vol.geo.count_of_clusters + 3;
    let mut overlay_served = 0;
    let mut base_served = 0;

    for c in 0..last {
        let want = vol.fat_entry(c);
        assert_eq!(vol.fat_entry_via_walk(c), want, "cluster {c}, fresh cursor");
        if c < vol.fat.next_free() {
            base_served += 1;
        }
    }
    // One cursor, ascending: the shape a real chain walk takes.
    {
        let mut walk = FatWalk::default();
        for c in 0..last {
            assert_eq!(
                vol.fat_entry_walked(c, &mut walk),
                vol.fat_entry(c),
                "cluster {c}, shared cursor ascending"
            );
            if walk.overlay.is_some() {
                overlay_served += 1;
            }
        }
    }
    // One cursor, descending: crosses every sector boundary the other way.
    {
        let mut walk = FatWalk::default();
        for c in (0..last).rev() {
            assert_eq!(
                vol.fat_entry_walked(c, &mut walk),
                vol.fat_entry(c),
                "cluster {c}, shared cursor descending"
            );
        }
    }
    assert!(
        overlay_served > 0,
        "the sweep never hit an overlay-written FAT sector, so it proves nothing \
         about the overlay arm"
    );
    assert!(base_served > 500, "want a volume worth sweeping");

    fs::remove_dir_all(&root).ok();
}

/// THE CHAIN THE RECONCILE PASS WALKS MUST BE THE CHAIN THE OLD PATH WALKED.
///
/// `chain_of` is what every reconcile-path walk now calls. Its result is compared
/// here, chain for chain, against `katea_write::chain` driven by the untouched
/// `fat_entry` -- including the deliberately fragmented and the deliberately
/// corrupt cases, where the answer is `None` and the pass HOLDS the file. A
/// disagreement on `None` would be the dangerous direction: it would let
/// reconcile act on a chain it should have held.
#[test]
fn chain_of_agrees_with_the_old_per_cluster_walk() {
    let (vol, root) = seeded_volume("chain_walk_identity");
    let max = vol.max_chain();
    let mut some = 0;
    let mut none = 0;

    // Past the mount's own clusters AND past everything the guest stamped above
    // them, so the sweep ends in free space where a chain must come back `None`.
    for first in 0..vol.fat.next_free() + 400 {
        let old = crate::katea_write::chain(first, max, |c| vol.fat_entry(c));
        let new = vol.chain_via_walk(first);
        assert_eq!(new, old, "chain from cluster {first}");
        match old {
            Some(_) => some += 1,
            None => none += 1,
        }
    }
    assert!(some > 100, "want real chains in the sweep, got {some}");
    assert!(
        none > 0,
        "the sweep never hit a held chain, so it proves nothing about the \
         hold path"
    );

    fs::remove_dir_all(&root).ok();
}

/// A CHAIN WALK MUST ASK THE STORE ONCE PER FAT SECTOR, NOT ONCE PER CLUSTER.
///
/// This is the O(folder) fix itself. Before the walk cursor, every cluster of
/// every chain paid a `SectorStore` lookup, a `RefCell` borrow and three
/// 512-byte copies to read four bytes; a 47 MB folder cost a 1.71 ms worst
/// projection pass and a 498 MB folder cost 18.47 ms, which is the visible-freeze
/// class. A FAT32 sector holds 128 entries, so a contiguous chain may resolve at
/// most `ceil(len / 128) + 1` sectors -- the `+ 1` covers a chain that starts
/// mid-sector.
///
/// Counter parity is asserted alongside, because the walk deliberately keeps
/// bumping `sector_reads` once per cluster: the profile field must not move.
///
/// NON-VACUOUS: routing `chain_of` back through `fat_entry` makes the resolve
/// count equal the cluster count and fails the ratio by two orders of magnitude.
#[test]
fn a_chain_walk_resolves_one_fat_sector_per_128_clusters() {
    let root = scratch("fat_walk_scaling");
    // One host file long enough that its chain crosses several FAT sectors.
    fs::write(root.join("BIG.BIN"), vec![7u8; 3 * 1024 * 1024]).unwrap();
    let vol = mount(&root);
    let first = first_cluster_of(&vol, &root.join("BIG.BIN"));

    vol.reset_fat_walk_resolves();
    let before = vol.storage_counters();
    let chain = vol.chain_via_walk(first).expect("a contiguous mount chain");
    let after = vol.storage_counters();

    let clusters = chain.len() as u64;
    assert!(clusters > 512, "want a long chain, got {clusters} clusters");
    let resolves = vol.fat_walk_resolves();
    assert!(
        resolves <= clusters.div_ceil(128) + 1,
        "{clusters} clusters resolved {resolves} FAT sectors: the walk is still \
         asking per cluster"
    );
    assert_eq!(
        after.sector_reads - before.sector_reads,
        clusters,
        "sector_reads must still count one served sector per cluster, or the \
         profile field moved under the fix"
    );

    fs::remove_dir_all(&root).ok();
}

/// The anti-clobber guard's definition, re-derived in the test from the live set
/// and the volume's own FAT, with no reference to how `ambiguous_by_full_scan`
/// is written:
///
/// > ambiguous = { k : ∃ c ∈ chain(k) with c a directory cluster, or c ∈ chain(k′)
/// >               for some live file k′ ≠ k }
fn ambiguous_by_definition(vol: &KateaTreeVolume) -> HashSet<(u32, [u8; 11])> {
    let max = vol.max_chain();
    // Union the chains of every live entry sharing a key -- a guest can write two
    // directory entries with the same 8.3 name, and the definition treats them as
    // one file, not as two files colliding.
    let mut chains: HashMap<(u32, [u8; 11]), HashSet<u32>> = HashMap::new();
    for (dir_cluster, name, first_cluster, is_dir) in vol.live_entries_for_test() {
        if is_dir || first_cluster < 2 {
            continue;
        }
        let Some(chain) = crate::katea_write::chain(first_cluster, max, |c| vol.fat_entry(c))
        else {
            continue;
        };
        chains.entry((dir_cluster, name)).or_default().extend(chain);
    }
    let mut out = HashSet::new();
    for (key, clusters) in &chains {
        let bad = clusters.iter().any(|c| {
            vol.is_directory_cluster(*c)
                || chains
                    .iter()
                    .any(|(other, theirs)| other != key && theirs.contains(c))
        });
        if bad {
            out.insert(*key);
        }
    }
    out
}

/// THE REFERENCE IS THE DEFINITION.
///
/// `ambiguous_by_full_scan` is the reference every faster implementation of the
/// anti-clobber guard is graded against, so it needs its own grading against
/// something that is not itself. `ambiguous_by_definition` above re-derives the
/// set from the live entries and the raw `fat_entry`, quadratically and without
/// looking at how the reference is written.
///
/// Driven over a mutation sequence that manufactures every shape the definition
/// distinguishes: two entries on one chain, a chain crossing a directory
/// cluster, a duplicate 8.3 name (which must NOT self-collide), and held chains.
///
/// NON-VACUOUS: the sweep asserts it produced both empty and non-empty answers,
/// and that at least one duplicate-name step occurred; dropping `other != key`
/// from the reference fails it on those steps.
#[test]
fn the_full_scan_reference_matches_the_declarative_definition() {
    let (mut vol, root) = seeded_volume("ambiguity_reference");
    let base = vol.fat.next_free();
    let mut seed = 0x0BAD_F00Du32;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };

    // SELF-COLLISION: two directory entries with the SAME 8.3 name AND the same
    // first cluster. Their chains are identical, so a definition that did not
    // exempt a key from colliding with itself would flag this key -- and phase 3
    // would hold a perfectly ordinary file forever.
    let self_key = {
        let before = vol.ambiguous_reference();
        stamp_file_entry_only(&mut vol, ROOT_CLUSTER, "SELF.BIN", ATTR_ARCHIVE, base, 4096);
        let one = vol.ambiguous_reference();
        stamp_file_entry_only(&mut vol, ROOT_CLUSTER, "SELF.BIN", ATTR_ARCHIVE, base, 4096);
        let two = vol.ambiguous_reference();
        assert_eq!(
            one, two,
            "a second entry with the same key and chain flagged it"
        );
        assert_eq!(one, ambiguous_by_definition(&vol));
        let mut name = [b' '; 11];
        name[..4].copy_from_slice(b"SELF");
        name[8..11].copy_from_slice(b"BIN");
        let _ = before;
        (ROOT_CLUSTER, name)
    };

    let mut nonempty = 0;
    let mut empty = 0;
    // Now a genuine two-key alias onto one chain, which MUST flag both.
    stamp_file_entry_only(
        &mut vol,
        ROOT_CLUSTER,
        "ALIAS.BIN",
        ATTR_ARCHIVE,
        base,
        4096,
    );
    let aliased = vol.ambiguous_reference();
    assert!(
        aliased.contains(&self_key),
        "two DIFFERENT keys on one chain must flag both, or the sweep below is \
         not testing the collision arm at all"
    );

    for step in 0..120 {
        let cluster = base + (rng() % 300);
        let value = match rng() % 4 {
            0 => 0u32,
            1 => crate::fat32::FAT32_EOC,
            2 => cluster + 1,
            _ => base + (rng() % 300),
        };
        let byte = cluster as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sec = vol.read_sector(lba);
        let off = byte % SECTOR;
        sec[off..off + 4].copy_from_slice(&(value & 0x0FFF_FFFF).to_le_bytes());
        vol.write_sector(lba, &sec);

        let reference = vol.ambiguous_reference();
        assert_eq!(
            reference,
            ambiguous_by_definition(&vol),
            "step {step}: the reference and the definition disagree"
        );
        if reference.is_empty() {
            empty += 1;
        } else {
            nonempty += 1;
        }
    }
    assert!(
        nonempty > 10,
        "the sweep never produced a flagged file, so it proves nothing ({empty} empty)"
    );

    fs::remove_dir_all(&root).ok();
}

/// A LINK PAST FAT COPY 0 MUST NOT BE MEMOIZED — THE ADVERSARIAL-GUEST CASE.
///
/// `fat_entry_walked` delegates a cluster whose entry falls outside FAT copy 0
/// to `fat_entry`, which resolves it through an LBA in FAT COPY 1. That is an
/// overlay sector the cursor never resolved, so it never lands in
/// `walk.sectors` — and a memo whose dependency set is missing a sector it
/// actually depends on would survive a guest write to that sector and serve a
/// chain the FAT no longer says. `walk.degraded = true` on that arm is what
/// stops the memo existing at all.
///
/// A guest can reach this deliberately: the FAT entry it writes is 28 bits wide
/// and nothing stops it naming a cluster past the synthesized FAT.
///
/// NON-VACUOUS, and this is the mutant the review found survivable: deleting the
/// `walk.degraded = true` line at the `within >= self.geo.fatsz` arm leaves the
/// whole katea suite green. It fails BOTH assertions here — the memo appears,
/// and the second walk then serves the pre-write chain.
#[test]
fn a_link_past_fat_copy_0_is_never_memoized() {
    let (mut vol, root) = seeded_volume("fat_copy0_escape");
    let fatsz = vol.geo.fatsz;
    let max = vol.max_chain();
    // The first cluster past what FAT copy 0 can address: 128 entries per sector.
    let past_copy0 = fatsz * 128;
    // A chain head deliberately NOT in FAT sector 0, so the copy-1 write below
    // cannot expire the memo through the head's own sector. 200 / 128 = 1.
    let head = 200u32;
    assert_ne!(head / 128, 0, "the head must not share sector 0");

    // The guest points `head` straight at a cluster past FAT copy 0.
    let byte = head as usize * 4;
    let head_lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
    let mut sec = vol.read_sector(head_lba);
    let off = byte % SECTOR;
    sec[off..off + 4].copy_from_slice(&(past_copy0 & 0x0FFF_FFFF).to_le_bytes());
    vol.write_sector(head_lba, &sec);

    let reference = |v: &KateaTreeVolume| crate::katea_write::chain(head, max, |c| v.fat_entry(c));
    let before = reference(&vol);
    assert_eq!(vol.chain_via_walk(head), before, "the walk must be exact");
    assert!(
        before.as_ref().is_some_and(|c| c.contains(&past_copy0)),
        "the fixture must actually reach a cluster past FAT copy 0, got {before:?}"
    );
    assert!(
        !vol.chain_memo_holds(head),
        "a walk that escaped FAT copy 0 was memoized: its dependency set cannot \
         name the copy-1 sector it actually read"
    );

    // Now write the FAT COPY 1 sector that `past_copy0` resolves through. It is
    // the sector at partition-relative `reserved + fatsz`, whose `within` is
    // 0 -- NOT the head's sector, so a memo of the walk above would still look
    // current.
    let copy1_lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + fatsz;
    let mut sec = vol.read_sector(copy1_lba);
    sec[0..4].copy_from_slice(&5u32.to_le_bytes());
    vol.write_sector(copy1_lba, &sec);

    let after = reference(&vol);
    assert_ne!(
        after, before,
        "the copy-1 write must change the chain, or the second assertion is vacuous"
    );
    assert_eq!(
        vol.chain_via_walk(head),
        after,
        "a stale memo served the pre-write chain across a FAT copy 1 write"
    );

    fs::remove_dir_all(&root).ok();
}

/// THE CHAIN MEMO MUST NEVER SERVE A CHAIN THE FAT NO LONGER SAYS.
///
/// This is the safety test for `ChainMemo`'s invalidation, and the dangerous
/// direction is obvious: a stale chain fed to reconcile writes the wrong bytes
/// into a real host file, or deletes one. So rather than assert the invalidation
/// rule, the test replays a deterministic pseudo-random sequence of guest FAT
/// writes -- links, EOCs, frees, and rewrites of sectors already written -- and
/// after EVERY ONE of them compares `chain_of` against `katea_write::chain`
/// driven by the untouched `fat_entry`, over every seeded first cluster.
///
/// The sequence deliberately mixes clusters that share a FAT sector with ones
/// that do not, so both arms of `chain_memo_is_current` are exercised: the
/// whole-volume epoch equality and the per-sector comparison.
///
/// NON-VACUOUS: deleting the `fat_sector_epoch.insert` in `note_metadata_write`
/// (so nothing ever expires) fails on the first write that changes a chain;
/// so does stamping the epoch but comparing it with `>=` instead of `<=`.
#[test]
fn the_chain_memo_never_outlives_a_fat_write_that_changes_its_chain() {
    let (mut vol, root) = seeded_volume("chain_memo_invalidation");
    let max = vol.max_chain();
    let base = vol.fat.next_free();
    // The clusters whose chains are checked after every mutation: mount-time
    // ones, guest-written ones, and free space past both.
    let watched: Vec<u32> = (2..12)
        .chain(base - 4..base + 320)
        .chain([base + 900, base + 1500])
        .collect();

    let mut seed = 0x1234_5678u32;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let mut changed_something = 0;

    for step in 0..250 {
        // Rewrite one FAT entry through the guest's own path.
        let cluster = base + (rng() % 340);
        let value = match rng() % 4 {
            0 => 0u32,                    // free it: chains through it end
            1 => crate::fat32::FAT32_EOC, // terminate here
            2 => cluster + 1,             // link forward
            _ => base + (rng() % 340),    // link anywhere, possibly a cycle
        };
        let byte = cluster as usize * 4;
        let lba = vol.geo.part_start + u32::from(RESERVED_SECTORS) + (byte / SECTOR) as u32;
        let mut sec = vol.read_sector(lba);
        let off = byte % SECTOR;
        let old = sec[off..off + 4].to_vec();
        sec[off..off + 4].copy_from_slice(&(value & 0x0FFF_FFFF).to_le_bytes());
        if old != sec[off..off + 4] {
            changed_something += 1;
        }
        vol.write_sector(lba, &sec);

        for &first in &watched {
            let want = crate::katea_write::chain(first, max, |c| vol.fat_entry(c));
            assert_eq!(
                vol.chain_via_walk(first),
                want,
                "step {step}: memo served a stale chain from cluster {first} \
                 after writing FAT[{cluster}] = {value:#x}"
            );
        }
    }
    assert!(
        changed_something > 100,
        "the sweep rewrote the same bytes every time ({changed_something} real \
         changes), so it proves nothing"
    );

    fs::remove_dir_all(&root).ok();
}

/// A PASS OVER AN UNCHANGED VOLUME MUST NOT WALK A SINGLE CHAIN.
///
/// This is the O(folder) fix's second half. `chain_of` is called three to four
/// times per live file per projection pass, and before the memo every one of
/// those stepped every cluster of the file. With nothing written to the FAT
/// since the last pass, none of them may walk at all.
///
/// NON-VACUOUS: returning early from `chain_of` before the memo lookup makes the
/// walk count equal the call count and fails the first assertion.
#[test]
fn a_second_pass_over_an_unchanged_volume_walks_no_chains() {
    let root = scratch("chain_memo_steady_state");
    for i in 0..8u32 {
        fs::write(root.join(format!("A{i:02}.BIN")), vec![i as u8; 384 * 1024]).unwrap();
    }
    let mut vol = mount(&root);
    let allocated = vol.fat.next_free() - ROOT_CLUSTER;
    assert!(allocated > 400, "want a volume worth walking");

    let first = vol.fat.next_free();
    stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "NEW.TXT",
        ATTR_ARCHIVE,
        first,
        b"hi",
    );
    vol.reconcile();
    assert_eq!(fs::read(root.join("NEW.TXT")).unwrap(), b"hi");

    // A second FULL pass with NO guest write in between: every chain the pass
    // asks for is one it already has, and none of the FAT has moved.
    vol.reset_chain_walk_counts();
    let before = vol.storage_counters();
    vol.reconcile();
    let after = vol.storage_counters();
    let (walks, hits) = vol.chain_walk_counts();

    assert!(
        hits > 0,
        "the second pass asked for no chains at all, so it proves nothing"
    );
    assert_eq!(
        walks, 0,
        "an unchanged volume re-walked {walks} chains ({hits} served from the \
         memo): the pass is still proportional to the folder"
    );
    // What the pass still reads is its DIRECTORY bytes, which is proportional to
    // the directory count, not to the data. Nothing near the cluster count.
    let reads = after.sector_reads - before.sector_reads;
    assert!(
        reads * 8 < u64::from(allocated),
        "the pass read {reads} sectors over {allocated} allocated clusters, so \
         something is still walking the FAT"
    );

    fs::remove_dir_all(&root).ok();
}

/// The counter block is now one `Cell` per field rather than one `Cell` over the
/// whole struct. The behaviour that has to survive is that a bump touches ITS
/// field and nothing else -- the old whole-block read-modify-write made that true
/// by construction, and per-field cells make it true by construction in a
/// different way, so it is worth pinning.
#[test]
fn a_counter_bump_moves_exactly_one_field() {
    let cells = KateaCounterCells::default();
    assert_eq!(
        cells.snapshot(),
        KateaStorageCounters::default(),
        "a fresh block must snapshot as the default report"
    );
    bump(&cells.host_read_operations, 3);
    let mut want = KateaStorageCounters {
        host_read_operations: 3,
        ..Default::default()
    };
    assert_eq!(cells.snapshot(), want, "one bump, one field");
    bump(&cells.projection_bytes, 512);
    want.projection_bytes = 512;
    assert_eq!(cells.snapshot(), want, "a second bump left the first alone");
    // Saturation, so an accumulating counter cannot wrap back through zero.
    bump(&cells.host_bytes, u64::MAX);
    bump(&cells.host_bytes, 1);
    want.host_bytes = u64::MAX;
    assert_eq!(cells.snapshot(), want);
}

/// A read-ahead HIT does no host I/O, so it must not enter the wall counters --
/// and, since it never uses one, must not pay for a clock read either. The
/// counters are what a test can see: `host_wall_ns` and `host_read_max_ns` are
/// frozen across a run of pure hits, which is only true while the `Instant` sits
/// BELOW the hit test.
#[test]
fn a_readahead_hit_does_not_enter_the_wall_counters() {
    let root = scratch("readahead_hit_wall");
    fs::write(root.join("SEQ.BIN"), vec![9u8; 512 * 1024]).unwrap();
    let vol = mount(&root);
    let first = first_cluster_of(&vol, &root.join("SEQ.BIN"));
    let base = vol.cluster_to_lba(first);

    // One physical read puts this sector's bytes in the read-ahead slot, keyed by
    // path and byte offset. Re-reading the SAME sector is then a guaranteed hit:
    // same path, offset 0, whole sector inside the buffer.
    let _ = vol.read_sector(base);
    let primed = vol.storage_counters();
    assert!(
        primed.host_read_operations > 0,
        "the priming read did no host I/O, so nothing was buffered"
    );
    for _ in 0..32 {
        let _ = vol.read_sector(base);
    }
    let after = vol.storage_counters();

    assert!(
        after.host_readahead_hits > primed.host_readahead_hits,
        "the sweep produced no read-ahead hits, so it proves nothing"
    );
    assert_eq!(
        after.host_read_operations, primed.host_read_operations,
        "a read-ahead hit performed a physical read"
    );
    assert_eq!(
        after.host_wall_ns, primed.host_wall_ns,
        "a read-ahead hit charged host wall time"
    );
    assert_eq!(
        after.host_read_max_ns, primed.host_read_max_ns,
        "a read-ahead hit moved the worst-single-read max"
    );

    fs::remove_dir_all(&root).ok();
}

/// The memo may only change WHETHER the bytes are recomputed, never WHAT they
/// are. Every FAT sector of the volume must read back byte for byte what an
/// uncached synthesis produces, in a read order that alternates between two
/// sectors so the single memo slot misses on every request.
#[test]
fn the_fat_memo_is_byte_identical_to_an_uncached_synthesis() {
    let (vol, root) = fresh_vol("fat_memo_identity");
    let reserved = u32::from(RESERVED_SECTORS);
    let fat_sectors = vol.geo.fatsz.min(64);

    for within in 0..fat_sectors {
        let uncached = vol.fat.fat_sector(within, &vol.geo);
        let lba = vol.geo.part_start + reserved + within;
        assert_eq!(vol.read_sector(lba), uncached, "FAT sector {within}");
        // Alternate to a different sector and back, so the next iteration and the
        // re-read below both come off a missed memo.
        let other = (within + 1) % fat_sectors;
        let _ = vol.read_sector(vol.geo.part_start + reserved + other);
        assert_eq!(
            vol.read_sector(lba),
            uncached,
            "FAT sector {within} re-read"
        );
    }
    // The second FAT copy is the same bytes at a different LBA.
    for within in 0..fat_sectors.min(4) {
        let lba = vol.geo.part_start + reserved + vol.geo.fatsz + within;
        assert_eq!(
            vol.read_sector(lba),
            vol.fat.fat_sector(within, &vol.geo),
            "FAT copy 2, sector {within}"
        );
    }

    fs::remove_dir_all(&root).ok();
}

/// The max counters must report the longest SINGLE operation, which is the only
/// figure that can tell a visible freeze from time spread thin.
#[test]
fn the_max_counters_track_the_longest_single_operation() {
    let root = scratch("max_counters");
    fs::write(root.join("BIG.BIN"), vec![0x33u8; 200_000]).unwrap();
    let mut vol = mount(&root);
    let lba = vol.cluster_to_lba(first_cluster_of(&vol, &root.join("BIG.BIN")));

    assert_eq!(
        vol.storage_counters().host_read_max_ns,
        0,
        "nothing has been read yet"
    );
    assert_eq!(vol.storage_counters().projection_max_ns, 0);

    read_command(&vol, lba, 8);
    let counters = vol.storage_counters();
    assert!(counters.host_read_max_ns > 0, "a host read was timed");
    assert!(
        counters.host_read_max_ns <= counters.host_wall_ns,
        "one operation cannot exceed the sum of all of them"
    );

    let first = vol.fat.next_free();
    stamp_file(
        &mut vol,
        ROOT_CLUSTER,
        "NEW.TXT",
        ATTR_ARCHIVE,
        first,
        b"hi",
    );
    vol.reconcile_after_write();
    let counters = vol.storage_counters();
    assert!(
        counters.projection_max_ns > 0,
        "a projection pass was timed"
    );
    assert!(
        counters.projection_max_ns <= counters.projection_wall_ns,
        "one pass cannot exceed the sum of all of them"
    );

    fs::remove_dir_all(&root).ok();
}

/// EVERY HOST MUTATION MUST LEAVE NOTHING CACHED FOR THE PATH IT TOUCHED.
///
/// `reconcile_mode` drops every cached read view on entry, but it then READS file
/// bytes -- phase 3 gathers a changing file from the base view before deciding to
/// rewrite it -- and only afterwards mutates. So the entry drop is not enough on
/// its own: by the time `atomic_write` replaces the file, the gather has already
/// re-opened it and filled a read-ahead slot with its pre-write bytes. On Windows
/// the stale handle stays perfectly valid and simply points at the replaced
/// content, and the buffer never notices at all.
///
/// This pins the post-condition for the overwrite path, which is the one a gather
/// can reach.
///
/// NON-VACUOUS: removing the scoped `invalidate_host_reads` from the `writes`
/// loop in `reconcile_mode` leaves the gather's handle and buffer in place and
/// fails both assertions.
#[test]
fn an_overwrite_leaves_no_cached_read_view_for_the_file() {
    let root = scratch("inval_overwrite");
    let path = root.join("VICTIM.BIN");
    // Two sectors, so the gather must read the second one from the HOST file:
    // the guest only writes the first, and a partly-written chain is what makes
    // reconcile open the base view at all.
    fs::write(&path, vec![0xAAu8; 2 * SECTOR]).unwrap();
    let mut vol = mount(&root);
    let lba = vol.cluster_to_lba(first_cluster_of(&vol, &path));

    vol.write_sector(lba, &[0x5Cu8; SECTOR]);
    vol.reconcile();

    assert_eq!(
        fs::read(&path).unwrap()[..SECTOR],
        [0x5Cu8; SECTOR],
        "the overwrite must have happened, or this proves nothing"
    );
    assert!(
        !vol.host_read_handle_cached(&path),
        "the gather's handle must not outlive the write it preceded"
    );
    assert!(
        !vol.readahead_holds(&path),
        "and neither must the bytes it read ahead"
    );

    fs::remove_dir_all(&root).ok();
}

/// The same post-condition for the rename path, on BOTH paths: the source, whose
/// cached views now describe a name that no longer exists, and the destination,
/// whose name has just acquired different content than anything cached under it
/// could have held.
///
/// HONEST ABOUT ITS STRENGTH: with the pass ordered as it is today, phase 2
/// applies renames before phase 3 reads anything, so the entry drop alone
/// already satisfies this and removing the scoped calls from the `renames` loop
/// does NOT fail it. The scoped calls are defence in depth for the ordering, and
/// this test is the post-condition they defend -- the one an ordering change
/// would break silently. `an_overwrite_leaves_no_cached_read_view_for_the_file`
/// is the reachable case, and its mutation does fail.
#[test]
fn a_rename_leaves_no_cached_read_view_for_either_path() {
    let root = scratch("inval_rename");
    let old_path = root.join("OLD.TXT");
    let new_path = root.join("NEW.TXT");
    // A file already on the host, so a read of it resolves through the host file
    // rather than out of the guest write store, and can prime the caches.
    fs::write(&old_path, b"keepme\r\n").unwrap();
    let mut vol = mount(&root);
    let first = first_cluster_of(&vol, &old_path);

    let lba = vol.cluster_to_lba(first);
    assert_eq!(&vol.read_sector(lba)[..6], b"keepme");
    // The handle cache is unconditional; the read-ahead is not, so this test
    // stays about invalidation in either leg of `IZARRAVM_HDD_READAHEAD`.
    assert!(vol.host_read_handle_cached(&old_path), "primed");

    rename_entry(&mut vol, ROOT_CLUSTER, "OLD.TXT", "NEW.TXT");
    vol.reconcile();

    assert!(!old_path.exists() && new_path.exists(), "the rename landed");
    for path in [&old_path, &new_path] {
        assert!(
            !vol.host_read_handle_cached(path),
            "a handle survived the rename: {}",
            path.display()
        );
        assert!(
            !vol.readahead_holds(path),
            "a read-ahead slot survived the rename: {}",
            path.display()
        );
    }
    assert_eq!(
        &vol.read_sector(lba)[..6],
        b"keepme",
        "and the bytes still resolve, now through the new name"
    );

    fs::remove_dir_all(&root).ok();
}

/// The same post-condition for the delete path. A handle left open here is worse
/// than stale: on Windows it keeps the deleted file's content readable through a
/// descriptor whose name is already gone.
///
/// Same standing as the rename post-condition above: satisfied today by the
/// entry drop as well, asserted so an ordering change cannot break it quietly.
#[test]
fn a_delete_leaves_no_cached_read_view_for_the_file() {
    let root = scratch("inval_delete");
    let path = root.join("GONE.TXT");
    // A file already on the host, so a read of it resolves through the host file
    // rather than out of the guest write store, and can prime the caches.
    fs::write(&path, b"bye\r\n").unwrap();
    let mut vol = mount(&root);
    let first = first_cluster_of(&vol, &path);

    let lba = vol.cluster_to_lba(first);
    assert_eq!(&vol.read_sector(lba)[..3], b"bye");
    // Unconditional, for the reason given in the rename test above.
    assert!(vol.host_read_handle_cached(&path), "primed");

    delete_entry(&mut vol, ROOT_CLUSTER, "GONE.TXT");
    free_chain(&mut vol, first);
    vol.reconcile();

    assert!(!path.exists(), "the delete landed");
    assert!(
        !vol.host_read_handle_cached(&path),
        "a handle survived the delete"
    );
    assert!(
        !vol.readahead_holds(&path),
        "a read-ahead slot survived the delete"
    );

    fs::remove_dir_all(&root).ok();
}
