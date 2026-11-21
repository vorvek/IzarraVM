// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

pub const EMBEDDED_SOUNDFONT_SHA256: &str =
    "cfcd66d89e8386823400eca64934b14fbea7bf48ba1f00d21189af1262794ec2";
const EMBEDDED_SOUNDFONT: &[u8] =
    include_bytes!("../../../third_party/fluidr3mono/FluidR3Mono_GM.sf3");
const FILE_NAME: &str =
    "FluidR3Mono_GM-cfcd66d89e8386823400eca64934b14fbea7bf48ba1f00d21189af1262794ec2.sf3";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set the per-user folder that holds the extracted SoundFont. The first call
/// wins. Without a call, the process temporary directory is the fallback.
pub fn set_soundfont_cache_dir(directory: PathBuf) {
    let _ = CACHE_DIR.set(directory);
}

/// The folder that holds the extracted SoundFont. A configured folder is used
/// as is. The shared temporary folder gets a fixed sub-folder.
fn cache_root(configured: Option<&Path>, temp: &Path) -> PathBuf {
    match configured {
        Some(directory) => directory.to_path_buf(),
        None => temp.join("izarravm").join("soundfonts"),
    }
}

/// Materialize the embedded SoundFont in the per-user folder.
///
/// FluidSynth loads SoundFonts by path. The content hash in the file name lets
/// compatible processes share the extracted file and makes an asset update
/// select a new cache entry. If the per-user folder cannot be written, the
/// process temporary directory is used instead.
pub fn embedded_soundfont_path() -> io::Result<PathBuf> {
    let temp = std::env::temp_dir();
    let configured = CACHE_DIR.get().map(PathBuf::as_path);
    let primary = cache_root(configured, &temp);
    match materialize_embedded_soundfont_in(&primary) {
        Ok(path) => Ok(path),
        Err(error) if configured.is_some() => {
            tracing::warn!(%error, "state folder is not writable; using the temporary directory");
            materialize_embedded_soundfont_in(&cache_root(None, &temp))
        }
        Err(error) => Err(error),
    }
}

fn materialize_embedded_soundfont_in(directory: &Path) -> io::Result<PathBuf> {
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
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() != expected.len() as u64 {
        return Ok(false);
    }

    reader_matches(file, expected)
}

/// Compare the bytes of `reader` with `expected`. A reader that holds more or
/// fewer bytes than `expected` does not match.
fn reader_matches(mut reader: impl Read, expected: &[u8]) -> io::Result<bool> {
    let mut offset = 0;
    let mut buffer = [0u8; 64 * 1024];
    while offset < expected.len() {
        let want = (expected.len() - offset).min(buffer.len());
        let count = reader.read(&mut buffer[..want])?;
        if count == 0 || buffer[..count] != expected[offset..offset + count] {
            return Ok(false);
        }
        offset += count;
    }
    // Any byte past the expected length means the source grew.
    Ok(reader.read(&mut buffer[..1])? == 0)
}

#[cfg(test)]
#[path = "soundfont_test.rs"]
mod tests;
