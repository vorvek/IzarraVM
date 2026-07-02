//! Shared FAT32 primitives for the "Katea" storage controller, plus the read-only
//! extractor that pulls the Toka-DOS system files back out of the committed
//! `crates/izarravm-firmware/roms/tokados-hdd.img`.
//!
//! The live host-folder disk is `katea_tree::KateaTreeVolume`. This module owns the
//! shared geometry and on-disk-format helpers it builds on — `sectors_per_cluster`,
//! `fat_size_sectors`, `lba_to_chs`, `stamp_fat32_bpb`, the 8.3 name codec
//! (`decode_83`), the `FileSource` enum, and the FAT32 BPB constants — together with
//! `extract_system_payload`, which reads the MBR, the partition VBR, and the root
//! files out of the embedded image so every mount can overlay the real KERNEL.SYS /
//! COMMAND.COM without re-vendoring them.
//!
//! Why these live here and not in `fat32.rs`: the FAT size MUST match the FreeDOS
//! kernel's `CalculateFATData` (initdisk.c), which yields **741** sectors for this
//! volume, NOT fatgen103's 746. The kernel computes a *default* BPB from the
//! partition size and trusts it until `bldbpb` reads our on-disk VBR; if the on-disk
//! FAT geometry disagrees, the two views of the data region diverge and the kernel
//! panics. 746-vs-741 was a real, gate-blocking boot bug, so `fat_size_sectors`
//! re-derives the kernel formula and never calls `fat32::fat32_geometry`.

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
    /// span at a time, in `katea_tree::read_source_span`.
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

/// The system payload pulled back out of a whole-disk FAT32 image: the two boot
/// sectors and the root files in directory order. This is what every Katea mount
/// overlays as its in-RAM system files (KERNEL.SYS, COMMAND.COM, ...).
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
/// like `tokados-hdd.img` — the inverse of the Python image builder. Returns the
/// MBR (LBA 0), the partition's VBR (the sector at `PART_START`), and the root
/// files in directory order.
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

        // TOKAMOUS.COM ships as a synthesized binary; the default AUTOEXEC loads it
        // HIGH into a TOKAEMM UMB (SP-4b M4).
        assert!(by_name.contains_key("TOKAMOUS.COM"), "TOKAMOUS.COM present");
        let autoexec = by_name.get("AUTOEXEC.BAT").expect("AUTOEXEC.BAT present");
        assert!(
            String::from_utf8_lossy(autoexec).contains("SET BLASTER=A220 I5 D1 H5 T6"),
            "default AUTOEXEC advertises the Sound Blaster"
        );
        assert!(
            String::from_utf8_lossy(autoexec).contains("LH TOKAMOUS"),
            "default AUTOEXEC loads the mouse driver high"
        );

        // SP-4b M4: TOKAEMM.SYS ships on the payload and the default CONFIG.SYS
        // loads it (frameless NOEMS) with DOS=HIGH,UMB — every default boot runs
        // FreeDOS in V86 under the guest memory manager.
        assert_eq!(
            by_name.get("TOKAEMM.SYS").map(|d| d.len()),
            Some(izarravm_firmware::tokaemm_sys().len()),
            "TOKAEMM.SYS on the payload matches the committed driver"
        );
        let config = by_name.get("CONFIG.SYS").expect("CONFIG.SYS present");
        let config_text = String::from_utf8_lossy(config);
        assert!(
            config_text.contains("DEVICE=C:\\TOKAEMM.SYS NOEMS"),
            "default CONFIG.SYS loads TOKAEMM"
        );
        assert!(
            config_text.contains("DOS=HIGH,UMB"),
            "default CONFIG.SYS uses the HMA + UMBs"
        );
        assert!(
            config_text.contains("LASTDRIVE=D"),
            "default CONFIG.SYS caps LASTDRIVE at D (A: floppy, C: HDD, D: CD)"
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
}
