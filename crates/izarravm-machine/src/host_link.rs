// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Test support: create a directory link on the host.
//!
//! On Windows the link is a directory junction, which needs no privilege.
//! On Unix it is a symbolic link.

use std::path::Path;

/// Make `link` a directory link that points at `target`. Panics if the host
/// refuses, so a test never passes without a real link.
pub(crate) fn make_dir_link(link: &Path, target: &Path) {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run cmd mklink");
        assert!(status.success(), "mklink /J failed for {}", link.display());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("create symlink");
}
