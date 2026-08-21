// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_input::ControllerConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DIRECTORY_NAME: &str = "Controller Profiles";
const PROFILE_EXTENSION: &str = "toml";
const PROFILE_FORMAT_VERSION: u32 = 1;
const NEW_PROFILE_NAME: &str = "New Profile";
const MAX_PROFILE_NAME_CHARS: usize = 80;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub(crate) struct ControllerProfileStore {
    directory: PathBuf,
}

#[derive(Debug)]
pub(crate) enum ControllerProfileError {
    InvalidName {
        name: String,
        reason: &'static str,
    },
    NotFound {
        name: String,
    },
    AlreadyExists {
        name: String,
    },
    NotAFile {
        name: String,
    },
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Serialize {
        name: String,
        source: Box<toml::ser::Error>,
    },
    Parse {
        name: String,
        path: PathBuf,
        source: Box<toml::de::Error>,
    },
    UnsupportedVersion {
        name: String,
        version: u32,
    },
    NameSpaceExhausted,
}

impl fmt::Display for ControllerProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { name, reason } => {
                write!(
                    formatter,
                    "invalid controller profile name {name:?}: {reason}"
                )
            }
            Self::NotFound { name } => {
                write!(formatter, "controller profile {name:?} does not exist")
            }
            Self::AlreadyExists { name } => {
                write!(formatter, "controller profile {name:?} already exists")
            }
            Self::NotAFile { name } => {
                write!(
                    formatter,
                    "controller profile {name:?} is not a regular file"
                )
            }
            Self::Io {
                action,
                path,
                source,
            } => write!(
                formatter,
                "could not {action} controller profile at {}: {source}",
                path.display()
            ),
            Self::Serialize { name, source } => {
                write!(
                    formatter,
                    "could not serialize controller profile {name:?}: {source}"
                )
            }
            Self::Parse { name, path, source } => write!(
                formatter,
                "could not parse controller profile {name:?} at {}: {source}",
                path.display()
            ),
            Self::UnsupportedVersion { name, version } => write!(
                formatter,
                "controller profile {name:?} uses unsupported format version {version}"
            ),
            Self::NameSpaceExhausted => {
                formatter.write_str("could not allocate a new controller profile name")
            }
        }
    }
}

impl std::error::Error for ControllerProfileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize { source, .. } => Some(source.as_ref()),
            Self::Parse { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredControllerProfile {
    format_version: u32,
    controller: ControllerConfig,
}

impl ControllerProfileStore {
    pub(crate) fn new(state_dir: impl AsRef<Path>) -> Self {
        Self {
            directory: state_dir.as_ref().join(DIRECTORY_NAME),
        }
    }

    pub(crate) fn list(&self) -> Result<Vec<String>, ControllerProfileError> {
        self.recover_backups()?;
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(self.io_error("list", &self.directory, source)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| self.io_error("list", &self.directory, source))?;
            let file_type = entry
                .file_type()
                .map_err(|source| self.io_error("inspect", &entry.path(), source))?;
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            if !has_profile_extension(&path) {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                continue;
            };
            if validate_profile_name(name).is_ok() {
                names.push(name.to_owned());
            }
        }
        names.sort_by(|left, right| {
            left.to_lowercase()
                .cmp(&right.to_lowercase())
                .then_with(|| left.cmp(right))
        });
        Ok(names)
    }

    pub(crate) fn load(&self, name: &str) -> Result<ControllerConfig, ControllerProfileError> {
        validate_profile_name(name)?;
        self.recover_backups()?;
        let path = self.resolve_existing_path(name)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| self.io_error("inspect", &path, source))?;
        if !metadata.file_type().is_file() {
            return Err(ControllerProfileError::NotAFile {
                name: name.to_owned(),
            });
        }
        let text =
            fs::read_to_string(&path).map_err(|source| self.io_error("read", &path, source))?;
        let stored: StoredControllerProfile =
            toml::from_str(&text).map_err(|source| ControllerProfileError::Parse {
                name: name.to_owned(),
                path: path.clone(),
                source: Box::new(source),
            })?;
        if stored.format_version != PROFILE_FORMAT_VERSION {
            return Err(ControllerProfileError::UnsupportedVersion {
                name: name.to_owned(),
                version: stored.format_version,
            });
        }
        Ok(stored.controller)
    }

    pub(crate) fn save(
        &self,
        name: &str,
        controller: &ControllerConfig,
    ) -> Result<(), ControllerProfileError> {
        validate_profile_name(name)?;
        let stored = StoredControllerProfile {
            format_version: PROFILE_FORMAT_VERSION,
            controller: controller.clone(),
        };
        let text = toml::to_string_pretty(&stored).map_err(|source| {
            ControllerProfileError::Serialize {
                name: name.to_owned(),
                source: Box::new(source),
            }
        })?;
        fs::create_dir_all(&self.directory)
            .map_err(|source| self.io_error("create directory for", &self.directory, source))?;
        self.recover_backups()?;
        let path = self
            .find_existing_path(name)?
            .unwrap_or_else(|| self.profile_path(name));
        self.atomic_write(&path, text.as_bytes())
    }

    pub(crate) fn create(
        &self,
        controller: &ControllerConfig,
    ) -> Result<String, ControllerProfileError> {
        let names = self.list()?;
        let mut ordinal = 1u64;
        loop {
            let candidate = if ordinal == 1 {
                NEW_PROFILE_NAME.to_owned()
            } else {
                format!("{NEW_PROFILE_NAME} {ordinal}")
            };
            if !names
                .iter()
                .any(|existing| profile_names_equal(existing, &candidate))
            {
                self.create_named(&candidate, controller)?;
                return Ok(candidate);
            }
            ordinal = ordinal
                .checked_add(1)
                .ok_or(ControllerProfileError::NameSpaceExhausted)?;
        }
    }

    pub(crate) fn create_named(
        &self,
        name: &str,
        controller: &ControllerConfig,
    ) -> Result<(), ControllerProfileError> {
        validate_profile_name(name)?;
        self.recover_backups()?;
        if self.find_existing_path(name)?.is_some() {
            return Err(ControllerProfileError::AlreadyExists {
                name: name.to_owned(),
            });
        }
        self.save(name, controller)
    }

    pub(crate) fn delete(&self, name: &str) -> Result<(), ControllerProfileError> {
        validate_profile_name(name)?;
        self.recover_backups()?;
        let path = self.resolve_existing_path(name)?;
        fs::remove_file(&path).map_err(|source| self.io_error("delete", &path, source))?;
        sync_directory(&self.directory);
        Ok(())
    }

    fn resolve_existing_path(&self, name: &str) -> Result<PathBuf, ControllerProfileError> {
        self.find_existing_path(name)?
            .ok_or_else(|| ControllerProfileError::NotFound {
                name: name.to_owned(),
            })
    }

    fn find_existing_path(&self, name: &str) -> Result<Option<PathBuf>, ControllerProfileError> {
        let exact = self.profile_path(name);
        match fs::symlink_metadata(&exact) {
            Ok(_) => return Ok(Some(exact)),
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(self.io_error("inspect", &exact, source)),
        }
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(self.io_error("inspect", &self.directory, source)),
        };
        for entry in entries {
            let entry =
                entry.map_err(|source| self.io_error("inspect", &self.directory, source))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|source| self.io_error("inspect", &path, source))?
                .is_file()
                || !has_profile_extension(&path)
            {
                continue;
            }
            let Some(existing) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if profile_names_equal(existing, name) {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn profile_path(&self, name: &str) -> PathBuf {
        self.directory.join(format!("{name}.{PROFILE_EXTENSION}"))
    }

    fn atomic_write(&self, path: &Path, contents: &[u8]) -> Result<(), ControllerProfileError> {
        let file_name = path.file_name().expect("a profile path has a file name");
        let (temporary, mut file) = self.create_temporary_file(file_name)?;
        if let Err(source) = file.write_all(contents).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(self.io_error("write", path, source));
        }
        drop(file);

        let backup = backup_path(path);
        let had_target = match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Ok(_) => {
                let _ = fs::remove_file(&temporary);
                return Err(ControllerProfileError::NotAFile {
                    name: path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => false,
            Err(source) => {
                let _ = fs::remove_file(&temporary);
                return Err(self.io_error("inspect", path, source));
            }
        };

        if had_target {
            if let Err(error) = self.remove_stale_backup(&backup) {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
            if let Err(source) = fs::rename(path, &backup) {
                let _ = fs::remove_file(&temporary);
                return Err(self.io_error("prepare to replace", path, source));
            }
        }
        if let Err(source) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            if had_target {
                let _ = fs::rename(&backup, path);
            }
            return Err(self.io_error("write", path, source));
        }
        if had_target {
            let _ = fs::remove_file(&backup);
        }
        sync_directory(&self.directory);
        Ok(())
    }

    fn create_temporary_file(
        &self,
        file_name: &std::ffi::OsStr,
    ) -> Result<(PathBuf, fs::File), ControllerProfileError> {
        for _ in 0..128 {
            let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary = self.directory.join(format!(
                ".{}.{}.{}.tmp",
                file_name.to_string_lossy(),
                std::process::id(),
                sequence
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => return Ok((temporary, file)),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(self.io_error("create", &temporary, source)),
            }
        }
        Err(self.io_error(
            "create",
            &self.directory,
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "could not allocate a temporary profile file",
            ),
        ))
    }

    fn recover_backups(&self) -> Result<(), ControllerProfileError> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(self.io_error("inspect", &self.directory, source)),
        };
        for entry in entries {
            let entry =
                entry.map_err(|source| self.io_error("inspect", &self.directory, source))?;
            let backup = entry.path();
            let Some(target_name) = backup_target_name(&backup) else {
                continue;
            };
            let target = self.directory.join(target_name);
            match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_file() => {
                    fs::remove_file(&backup).map_err(|source| {
                        self.io_error("remove stale backup for", &target, source)
                    })?;
                }
                Ok(_) => {
                    return Err(self.io_error(
                        "recover",
                        &target,
                        io::Error::new(io::ErrorKind::InvalidData, "profile target is not a file"),
                    ));
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => {
                    let metadata = fs::symlink_metadata(&backup)
                        .map_err(|source| self.io_error("inspect", &backup, source))?;
                    if !metadata.file_type().is_file() {
                        return Err(self.io_error(
                            "recover",
                            &backup,
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                "profile backup is not a file",
                            ),
                        ));
                    }
                    fs::rename(&backup, &target)
                        .map_err(|source| self.io_error("recover", &target, source))?;
                    sync_directory(&self.directory);
                }
                Err(source) => return Err(self.io_error("inspect", &target, source)),
            }
        }
        Ok(())
    }

    fn remove_stale_backup(&self, backup: &Path) -> Result<(), ControllerProfileError> {
        match fs::remove_file(backup) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(self.io_error("remove stale backup", backup, source)),
        }
    }

    fn io_error(
        &self,
        action: &'static str,
        path: &Path,
        source: io::Error,
    ) -> ControllerProfileError {
        ControllerProfileError::Io {
            action,
            path: path.to_owned(),
            source,
        }
    }
}

fn has_profile_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(PROFILE_EXTENSION))
}

fn backup_path(path: &Path) -> PathBuf {
    let file_name = path.file_name().expect("a profile path has a file name");
    path.with_file_name(format!(".{}.backup", file_name.to_string_lossy()))
}

fn backup_target_name(path: &Path) -> Option<&str> {
    let file_name = path.file_name()?.to_str()?;
    let target = file_name.strip_prefix('.')?.strip_suffix(".backup")?;
    let target_path = Path::new(target);
    if target_path.file_name()?.to_str()? != target || !has_profile_extension(target_path) {
        return None;
    }
    let name = target_path.file_stem()?.to_str()?;
    validate_profile_name(name).is_ok().then_some(target)
}

fn profile_names_equal(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

fn validate_profile_name(name: &str) -> Result<(), ControllerProfileError> {
    let invalid = |reason| ControllerProfileError::InvalidName {
        name: name.to_owned(),
        reason,
    };
    if name.is_empty() {
        return Err(invalid("the name is empty"));
    }
    if name.trim() != name {
        return Err(invalid("remove spaces at the start or end"));
    }
    if name.starts_with('.') {
        return Err(invalid("the name cannot start with a dot"));
    }
    if name.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(invalid("the name is too long"));
    }
    if name.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
    }) {
        return Err(invalid(
            "the name contains a character that cannot be used in a file name",
        ));
    }
    if name.ends_with('.') {
        return Err(invalid("the name cannot end with a dot"));
    }
    if is_windows_reserved_name(name) {
        return Err(invalid("the name is reserved by the operating system"));
    }
    Ok(())
}

fn is_windows_reserved_name(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let bytes = base.as_bytes();
    bytes.len() == 4 && matches!(&bytes[..3], b"COM" | b"LPT") && bytes[3].is_ascii_digit()
}

#[cfg(unix)]
fn sync_directory(directory: &Path) {
    if let Ok(directory) = fs::File::open(directory) {
        let _ = directory.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) {}

#[cfg(test)]
#[path = "controller_profiles_test.rs"]
mod tests;
