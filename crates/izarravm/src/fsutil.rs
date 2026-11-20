// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Small file helpers shared by the binary.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Read a whole file. The error text names the path and keeps the error kind.
pub fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

/// Write `bytes` to `path` through a temporary file in the same folder.
///
/// The temporary file is written and synced first, then renamed over the
/// target, so a crash cannot leave a half-written target. If the temporary
/// file cannot be made or the rename fails, the target is written directly
/// and the temporary file is removed.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(temporary) = write_temporary(path, bytes) else {
        return fs::write(path, bytes);
    };
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            let result = fs::write(path, bytes);
            let _ = fs::remove_file(&temporary);
            result
        }
    }
}

/// Write `bytes` to `path` in place and flush them to the device.
pub fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make a synced temporary file beside `path`. `None` when that fails.
fn write_temporary(path: &Path, bytes: &[u8]) -> Option<std::path::PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_file_name(format!(".{name}.{}.{sequence}.tmp", std::process::id()));
    let written = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
        return None;
    }
    Some(temporary)
}

#[cfg(test)]
#[path = "fsutil_test.rs"]
mod tests;
