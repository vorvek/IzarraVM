// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Path checks that keep the write targets of this tool away from the corpus.
//!
//! The corpus is read-only. `translate` writes into the extract root and into
//! the `--output` file. These checks refuse a write target that lies inside the
//! corpus, contains the corpus, or contains the `dosbox.conf` being read.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

fn refuse(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Canonical form of a path that may not exist yet. The nearest existing
/// ancestor is canonicalized and the missing tail is appended. A `..` in the
/// missing tail is refused, because it could climb back out of the checked
/// location.
pub fn canonical_lenient(path: &Path) -> io::Result<PathBuf> {
    let mut tail: Vec<OsString> = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        if let Ok(mut base) = current.canonicalize() {
            for part in tail.iter().rev() {
                base.push(part);
            }
            return Ok(base);
        }
        let name = current.file_name().map(OsString::from);
        let parent = current.parent().map(Path::to_path_buf);
        match (name, parent) {
            (Some(name), Some(parent)) => {
                tail.push(name);
                current = parent;
            }
            _ => {
                let has_parent_dir = current
                    .components()
                    .any(|component| component == Component::ParentDir);
                let why = if has_parent_dir {
                    "a missing path with a `..` component"
                } else {
                    "a path with no existing ancestor"
                };
                return Err(refuse(format!("refusing {why}: {}", path.display())));
            }
        }
    }
}

/// The corpus root that holds `conf`: the parent of the nearest `!dos`
/// ancestor. The corpus layout is `<root>\!dos\<short>\dosbox.conf`.
fn corpus_root_of_conf(conf: &Path) -> Option<PathBuf> {
    conf.ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("!dos"))
        })
        .and_then(Path::parent)
        .map(Path::to_path_buf)
}

/// The corpus root for a `translate` run: the explicit `--exodos-root` when
/// given, otherwise the one the conf path implies, when it has one.
pub fn corpus_root(conf: &Path, exodos_root: Option<&Path>) -> io::Result<Option<PathBuf>> {
    match exodos_root {
        Some(root) => Ok(Some(canonical_lenient(root)?)),
        None => Ok(corpus_root_of_conf(&conf.canonicalize()?)),
    }
}

fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Refuse an extract root that contains the conf, lies inside the conf's
/// directory, or overlaps the corpus root.
pub fn check_extract_root(
    conf: &Path,
    extract_root: &Path,
    exodos_root: Option<&Path>,
) -> io::Result<()> {
    let conf_canonical = conf.canonicalize()?;
    let extract = canonical_lenient(extract_root)?;
    if conf_canonical.starts_with(&extract) {
        return Err(refuse(format!(
            "the extract root {} contains the conf {}",
            extract_root.display(),
            conf.display()
        )));
    }
    if conf_canonical
        .parent()
        .is_some_and(|dir| extract.starts_with(dir))
    {
        return Err(refuse(format!(
            "the extract root {} lies inside the folder of the conf {}",
            extract_root.display(),
            conf.display()
        )));
    }
    if let Some(root) = corpus_root(conf, exodos_root)?
        && overlaps(&extract, &root)
    {
        return Err(refuse(format!(
            "the extract root {} overlaps the corpus root {}",
            extract_root.display(),
            root.display()
        )));
    }
    Ok(())
}

/// Refuse an output path that lies inside the corpus root.
pub fn check_output(output: &Path, corpus_root: Option<&Path>) -> io::Result<()> {
    let Some(root) = corpus_root else {
        return Ok(());
    };
    if canonical_lenient(output)?.starts_with(root) {
        return Err(refuse(format!(
            "the output path {} is inside the corpus root {}",
            output.display(),
            root.display()
        )));
    }
    Ok(())
}

/// The corpus root a `--dos-root` scan implies: the parent of a `!dos` folder,
/// or the scanned folder itself.
pub fn census_corpus_root(dos_root: &Path) -> io::Result<PathBuf> {
    let canonical = dos_root.canonicalize()?;
    let is_bang_dos = canonical
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("!dos"));
    match canonical.parent() {
        Some(parent) if is_bang_dos => Ok(parent.to_path_buf()),
        _ => Ok(canonical),
    }
}

#[cfg(test)]
#[path = "guard_test.rs"]
mod tests;
