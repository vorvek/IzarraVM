// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("izarravm-fsutil-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn temp_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        .count()
}

#[test]
fn read_file_error_names_the_path_and_keeps_the_kind() {
    let dir = temp_dir("read");
    let missing = dir.join("no-such-file.bin");
    let error = read_file(&missing).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
    assert!(
        error.to_string().contains("no-such-file.bin"),
        "error text was {error}"
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn atomic_write_replaces_the_target_and_leaves_no_temporary_file() {
    let dir = temp_dir("replace");
    let target = dir.join("data.bin");
    fs::write(&target, b"old contents that are longer").unwrap();
    atomic_write(&target, b"new").unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"new");
    assert_eq!(temp_files(&dir), 0);
    atomic_write(&dir.join("fresh.bin"), b"x").unwrap();
    assert_eq!(fs::read(dir.join("fresh.bin")).unwrap(), b"x");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn atomic_write_failure_returns_the_error_and_removes_the_temporary_file() {
    let dir = temp_dir("fail");
    let target = dir.join("is-a-folder");
    fs::create_dir(&target).unwrap();
    assert!(atomic_write(&target, b"data").is_err());
    assert_eq!(temp_files(&dir), 0);
    assert!(target.is_dir());
    fs::remove_dir_all(&dir).unwrap();
}
