//! toka-pack: bundle the built Toka-DOS binaries into `tokados.rom`.
//!
//! Usage: `toka-pack <build-dir> <output-rom>`.
//!
//! The build dir holds the Open Watcom `.com`/`.exe` tools plus `tokaboot.bin`.
//! Every tool becomes a system file (installed onto C:); the boot record is
//! flagged so the installer skips it but the machine can still find it by name.
//! Aliases (COMMAND.COM and friends) are emitted as extra directory entries.
//!
//! The format matches `izarravm_firmware::toka_rom`:
//! ```text
//! 0  4    magic "TOKA"
//! 4  2    version u16 LE (1)
//! 6  2    file count u16 LE
//! 8  2    reserved (0)
//! 10 n*20 directory entries: name[11], flags u8, off u32 LE, len u32 LE
//! ...     file data
//! ```

use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"TOKA";
const VERSION: u16 = 1;
const HEADER_LEN: usize = 10;
const ENTRY_LEN: usize = 20;
const BUDGET: usize = 1024 * 1024;

/// flags bit0: a system file the installer lays onto C:.
const FLAG_SYSTEM: u8 = 0x01;

/// The boot record's reserved name. The machine looks this up to place it at
/// 0x7C00; it carries no system flag, so the installer does not write it to C:.
const BOOT_RECORD: &str = "TOKABOOT.BIN";

/// Duplicate-name aliases: (canonical, alias). Emitted only when the canonical
/// file is present in the build.
const ALIASES: &[(&str, &str)] = &[
    ("IZCMD.COM", "COMMAND.COM"),
    ("IZCDEX.COM", "MSCDEX.COM"),
    ("IZBASIC.COM", "BASIC.COM"),
    ("EDITOR.COM", "EDIT.COM"),
    ("IZMOUSE.COM", "MOUSE.COM"),
];

struct PackFile {
    name: String,
    flags: u8,
    data: Vec<u8>,
}

/// Encode "IZCMD.COM" into the 11-byte 8.3 field "IZCMD   COM".
fn pack_8_3(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    for (i, b) in base.bytes().take(8).enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().take(3).enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    out
}

fn collect_tools(build: &Path) -> Vec<PackFile> {
    let mut files = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(build)
        .expect("read build dir")
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort(); // deterministic ROM layout
    for path in entries {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_uppercase());
        if matches!(ext.as_deref(), Some("COM") | Some("EXE")) {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("file stem")
                .to_ascii_uppercase();
            let name = format!("{}.{}", stem, ext.unwrap());
            let data = std::fs::read(&path).expect("read tool");
            files.push(PackFile {
                name,
                flags: FLAG_SYSTEM,
                data,
            });
        }
    }
    files
}

fn build_blob(files: &[PackFile]) -> Vec<u8> {
    let count = files.len();
    let dir_start = HEADER_LEN;
    let data_start = HEADER_LEN + count * ENTRY_LEN;

    let mut blob = Vec::new();
    blob.extend_from_slice(MAGIC);
    blob.extend_from_slice(&VERSION.to_le_bytes());
    blob.extend_from_slice(&(count as u16).to_le_bytes());
    blob.extend_from_slice(&0u16.to_le_bytes());
    blob.resize(data_start, 0);

    let mut offsets = Vec::with_capacity(count);
    for file in files {
        offsets.push(blob.len());
        blob.extend_from_slice(&file.data);
    }

    for (i, file) in files.iter().enumerate() {
        let e = dir_start + i * ENTRY_LEN;
        blob[e..e + 11].copy_from_slice(&pack_8_3(&file.name));
        blob[e + 11] = file.flags;
        blob[e + 12..e + 16].copy_from_slice(&(offsets[i] as u32).to_le_bytes());
        blob[e + 16..e + 20].copy_from_slice(&(file.data.len() as u32).to_le_bytes());
    }
    blob
}

/// Re-parse the blob and confirm every file round-trips. Cheap insurance that
/// the writer agrees with the firmware reader.
fn verify(blob: &[u8], files: &[PackFile]) {
    assert_eq!(&blob[0..4], MAGIC, "magic");
    let count = u16::from_le_bytes([blob[6], blob[7]]) as usize;
    assert_eq!(count, files.len(), "file count");
    for (i, file) in files.iter().enumerate() {
        let e = HEADER_LEN + i * ENTRY_LEN;
        let off = u32::from_le_bytes([blob[e + 12], blob[e + 13], blob[e + 14], blob[e + 15]])
            as usize;
        let len = u32::from_le_bytes([blob[e + 16], blob[e + 17], blob[e + 18], blob[e + 19]])
            as usize;
        assert_eq!(&blob[off..off + len], file.data.as_slice(), "data for {}", file.name);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: toka-pack <build-dir> <output-rom>");
        std::process::exit(2);
    }
    let build = PathBuf::from(&args[1]);
    let out = PathBuf::from(&args[2]);

    let mut files = collect_tools(&build);

    // The boot record, flagged so the installer skips it.
    let boot = std::fs::read(build.join("tokaboot.bin")).expect("read tokaboot.bin");
    files.push(PackFile {
        name: BOOT_RECORD.to_string(),
        flags: 0,
        data: boot,
    });

    // Alias duplicates for the canonical files that exist.
    let mut aliases = Vec::new();
    for (canonical, alias) in ALIASES {
        if let Some(src) = files.iter().find(|f| f.name.eq_ignore_ascii_case(canonical)) {
            aliases.push(PackFile {
                name: (*alias).to_string(),
                flags: src.flags,
                data: src.data.clone(),
            });
        }
    }
    files.extend(aliases);

    let blob = build_blob(&files);
    assert!(
        blob.len() <= BUDGET,
        "tokados.rom is {} bytes, over the {} byte budget",
        blob.len(),
        BUDGET
    );
    verify(&blob, &files);

    std::fs::write(&out, &blob).expect("write rom");
    println!(
        "packed {} files into {} ({} bytes)",
        files.len(),
        out.display(),
        blob.len()
    );
}
