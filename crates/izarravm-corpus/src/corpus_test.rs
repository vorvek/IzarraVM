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
