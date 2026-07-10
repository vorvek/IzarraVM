// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const EMBEDDED_SOUNDFONT_SHA256: &str =
    "cfcd66d89e8386823400eca64934b14fbea7bf48ba1f00d21189af1262794ec2";
const EMBEDDED_SOUNDFONT: &[u8] =
    include_bytes!("../../../third_party/fluidr3mono/FluidR3Mono_GM.sf3");
const FILE_NAME: &str =
    "FluidR3Mono_GM-cfcd66d89e8386823400eca64934b14fbea7bf48ba1f00d21189af1262794ec2.sf3";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Materialize the embedded SoundFont in the process temporary directory.
///
/// FluidSynth loads SoundFonts by path. The content hash in the file name lets
/// compatible processes share the extracted file and makes an asset update
/// select a new cache entry.
pub fn embedded_soundfont_path() -> io::Result<PathBuf> {
    materialize_embedded_soundfont_in(&std::env::temp_dir())
}

fn materialize_embedded_soundfont_in(cache_root: &Path) -> io::Result<PathBuf> {
    let directory = cache_root.join("izarravm").join("soundfonts");
    fs::create_dir_all(&directory)?;
    let path = directory.join(FILE_NAME);
    if file_matches(&path, EMBEDDED_SOUNDFONT)? {
        return Ok(path);
    }

    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(
        ".{FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    fs::write(&temporary, EMBEDDED_SOUNDFONT)?;

    if file_matches(&path, EMBEDDED_SOUNDFONT)? {
        fs::remove_file(&temporary)?;
        return Ok(path);
    }
    if path.exists() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    match fs::rename(&temporary, &path) {
        Ok(()) => Ok(path),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            if file_matches(&path, EMBEDDED_SOUNDFONT)? {
                Ok(path)
            } else {
                Err(error)
            }
        }
    }
}

fn file_matches(path: &Path, expected: &[u8]) -> io::Result<bool> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(false);
    }

    let mut offset = 0;
    let mut buffer = [0u8; 64 * 1024];
    while offset < expected.len() {
        let count = file.read(&mut buffer)?;
        if count == 0 || buffer[..count] != expected[offset..offset + count] {
            return Ok(false);
        }
        offset += count;
    }
    Ok(true)
}

#[cfg(test)]
#[path = "soundfont_test.rs"]
mod tests;
