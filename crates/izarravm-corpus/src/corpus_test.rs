// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn detects_dos_launchers_case_insensitively() {
    assert_eq!(
        launcher_kind(Path::new("GAME.EXE")),
        Some(LauncherKind::Exe)
    );
    assert_eq!(
        launcher_kind(Path::new("START.bat")),
        Some(LauncherKind::Bat)
    );
    assert_eq!(launcher_kind(Path::new("README.TXT")), None);
}

#[test]
fn an_empty_environment_value_is_no_root() {
    let error = resolve_root(None, Some(OsString::new())).unwrap_err();
    assert!(error.contains("--corpus-root"));
    assert!(error.contains("IZARRAVM_CORPUS_ROOT"));
}

#[test]
fn no_flag_and_no_variable_is_an_error_without_a_default_path() {
    assert!(resolve_root(None, None).is_err());
}

#[test]
fn the_flag_wins_over_the_variable() {
    let root = resolve_root(Some(PathBuf::from("flag")), Some(OsString::from("env")));
    assert_eq!(root.unwrap(), PathBuf::from("flag"));
}

#[test]
fn the_variable_is_used_without_a_flag() {
    let root = resolve_root(None, Some(OsString::from("env")));
    assert_eq!(root.unwrap(), PathBuf::from("env"));
}
