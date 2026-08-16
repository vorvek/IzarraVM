// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! A case-insensitive index of one extracted game tree.
//!
//! The eXo confs cannot be trusted about the layout (see the design's launch
//! target resolution section: `Borderwo` does `cd Borderwo` into a directory
//! that does not exist, and DOSBox carries on anyway), so every path decision
//! the translator makes is resolved against the real files instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One directory's contents, keyed by lowercased name.
#[derive(Debug, Default, Clone)]
pub struct DirEntryIndex {
    /// lowercased name -> on-disk name.
    pub files: BTreeMap<String, String>,
    /// lowercased name -> on-disk name.
    pub dirs: BTreeMap<String, String>,
}

/// The whole tree, keyed by lowercased directory path relative to the root,
/// with the root itself under the empty string. Separators are `/`.
#[derive(Debug, Default, Clone)]
pub struct Tree {
    pub root: PathBuf,
    pub dirs: BTreeMap<String, DirEntryIndex>,
    /// Deepest directory nesting seen, root counting as 0.
    pub max_depth: usize,
    /// Files at or above 4 GiB, which Katea skips.
    pub oversize_files: Vec<String>,
    /// Names that are not already 8.3-clean, so Katea's folding would rename them.
    pub non_83_names: Vec<String>,
}

/// Deepest directory nesting the index walks. FAT itself has no depth limit and
/// the DOS path buffer runs out long before this, so a tree past it is one the
/// translator refuses anyway (`tree-too-deep`). The bound is enforced INSIDE the
/// walk rather than checked after it: the walk is recursive, and a junction
/// loop on the host would otherwise blow the stack before anyone could refuse
/// the title.
pub const MAX_TREE_DEPTH: usize = 32;

impl Tree {
    pub fn index(root: &Path) -> std::io::Result<Tree> {
        let mut tree = Tree {
            root: root.to_path_buf(),
            ..Tree::default()
        };
        tree.walk(root, "", 0)?;
        Ok(tree)
    }

    fn walk(&mut self, dir: &Path, rel: &str, depth: usize) -> std::io::Result<()> {
        self.max_depth = self.max_depth.max(depth);
        if depth > MAX_TREE_DEPTH {
            // Stop here and leave `max_depth` past the limit, which is what
            // makes the caller refuse the title. Nothing below this point can
            // be reached by a DOS path anyway.
            return Ok(());
        }
        let mut index = DirEntryIndex::default();
        let mut subdirs = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let meta = entry.metadata()?;
            if meta.is_symlink() {
                continue;
            }
            if !is_83_clean(&name) {
                self.non_83_names.push(join_rel(rel, &name));
            }
            if meta.is_dir() {
                index.dirs.insert(name.to_ascii_lowercase(), name.clone());
                subdirs.push((entry.path(), join_rel(rel, &name)));
            } else {
                if meta.len() >= 4 * 1024 * 1024 * 1024 {
                    self.oversize_files.push(join_rel(rel, &name));
                }
                index.files.insert(name.to_ascii_lowercase(), name.clone());
            }
        }
        self.dirs.insert(rel.to_ascii_lowercase(), index);
        for (path, child_rel) in subdirs {
            self.walk(&path, &child_rel, depth + 1)?;
        }
        Ok(())
    }

    pub fn dir(&self, rel: &str) -> Option<&DirEntryIndex> {
        self.dirs.get(&normalize_rel(rel).to_ascii_lowercase())
    }

    /// Does `name` exist as a file in `rel`? Returns the on-disk name.
    pub fn file_in(&self, rel: &str, name: &str) -> Option<&str> {
        self.dir(rel)?
            .files
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Does `name` exist as a directory in `rel`? Returns the on-disk name.
    pub fn subdir_in(&self, rel: &str, name: &str) -> Option<&str> {
        self.dir(rel)?
            .dirs
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Every directory path in the tree, breadth-first from the root.
    pub fn dirs_breadth_first(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.dirs.keys().cloned().collect();
        paths.sort_by_key(|p| {
            (
                p.matches('/').count() + usize::from(!p.is_empty()),
                p.clone(),
            )
        });
        paths
    }

    /// Resolve a DOS-ish relative path (`.\sb16`, `..`, `DUKE3D\CD`) against
    /// `cwd`, returning the new relative path when every component exists.
    pub fn resolve_dir(&self, cwd: &str, spec: &str) -> Option<String> {
        let mut current = normalize_rel(cwd);
        for part in spec.split(['\\', '/']) {
            let part = part.trim();
            if part.is_empty() || part == "." {
                continue;
            }
            if part == ".." {
                current = match current.rfind('/') {
                    Some(cut) => current[..cut].to_string(),
                    None => String::new(),
                };
                continue;
            }
            let actual = self.subdir_in(&current, part)?;
            current = join_rel(&current, actual);
        }
        Some(current)
    }

    /// Does a DOS wildcard pattern match anything in `cwd`? Used to evaluate
    /// the `if not exist *.sel goto menu` line the eXo launcher template opens
    /// with; a freshly extracted tree has no `.sel`, so the menu branch is the
    /// one a real first run takes too.
    pub fn exists_pattern(&self, cwd: &str, spec: &str) -> bool {
        let spec = spec.trim().trim_matches('"');
        let (dir_part, name_part) = match spec.rfind(['\\', '/']) {
            Some(cut) => (&spec[..cut], &spec[cut + 1..]),
            None => ("", spec),
        };
        let Some(dir) = self.resolve_dir(cwd, dir_part) else {
            return false;
        };
        if !name_part.contains(['*', '?']) {
            return self.file_in(&dir, name_part).is_some()
                || self.subdir_in(&dir, name_part).is_some();
        }
        let Some(index) = self.dir(&dir) else {
            return false;
        };
        index
            .files
            .keys()
            .chain(index.dirs.keys())
            .any(|name| wildcard_match(&name_part.to_ascii_lowercase(), name))
    }
}

/// DOS 8.3 wildcard match against an already-lowercased name.
pub fn wildcard_match(pattern: &str, name: &str) -> bool {
    let (pat_stem, pat_ext) = split_83(pattern);
    let (name_stem, name_ext) = split_83(name);
    glob_part(pat_stem, name_stem) && glob_part(pat_ext, name_ext)
}

fn split_83(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(cut) => (&name[..cut], &name[cut + 1..]),
        None => (name, ""),
    }
}

fn glob_part(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let mut p = 0;
    let mut t = 0;
    while p < pattern.len() {
        match pattern[p] {
            // DOS `*` inside a stem or extension swallows the rest of it.
            '*' => return true,
            '?' => {
                if t < text.len() {
                    t += 1;
                }
                p += 1;
            }
            c => {
                if t >= text.len() || text[t] != c {
                    return false;
                }
                t += 1;
                p += 1;
            }
        }
    }
    t == text.len()
}

/// Is this name already what Katea's FAT folding would produce, so that the
/// folding is the identity for it? Uppercase is not required, since the fold
/// only upcases; spaces, extra dots and over-long parts are what rename a file.
pub fn is_83_clean(name: &str) -> bool {
    let (stem, ext) = match name.rfind('.') {
        Some(cut) => (&name[..cut], &name[cut + 1..]),
        None => (name, ""),
    };
    if stem.is_empty() || stem.len() > 8 || ext.len() > 3 {
        return false;
    }
    let ok = |s: &str| {
        s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_^$~!#%&-{}()@'`".contains(c))
    };
    ok(stem) && ok(ext)
}

/// A directory name is 8.3-clean only when it carries no dot at all. Katea
/// treats a directory's whole name as the stem and strips dots from it, so a
/// host `SB16.CD` is served to the guest as `SB16CD` and a generated
/// `cd SB16.CD` would fail.
pub fn is_83_clean_dir(name: &str) -> bool {
    !name.contains('.') && is_83_clean(name)
}

/// Names Katea reserves at the mount root for the synthetic Toka-DOS overlay.
/// A game shipping its own root-level `DOS` folder gets folded to `DOS~1`
/// without an error, and `cd \DOS` then lands in the system folder instead.
pub const RESERVED_ROOT_NAMES: [&str; 3] = ["dos", "kernel.sys", "command.com"];

impl Tree {
    /// Root-level names that collide with the Katea overlay's own.
    pub fn reserved_root_collisions(&self) -> Vec<String> {
        let Some(root) = self.dir("") else {
            return Vec::new();
        };
        root.dirs
            .keys()
            .chain(root.files.keys())
            .filter(|name| RESERVED_ROOT_NAMES.contains(&name.as_str()))
            .cloned()
            .collect()
    }

    /// Would FAT folding rename `name` inside `rel`? True when the name is not
    /// already 8.3-clean, or when a sibling is not 8.3-clean and could therefore
    /// be folded onto the same 11 bytes and win the collision by sort order.
    /// Deliberately conservative: it does not reimplement the fold, it only
    /// refuses to promise fidelity where the fold has anything to do.
    pub fn fold_is_identity(&self, rel: &str, name: &str, is_dir: bool) -> bool {
        let clean = if is_dir {
            is_83_clean_dir(name)
        } else {
            is_83_clean(name)
        };
        if !clean {
            return false;
        }
        let Some(index) = self.dir(rel) else {
            return false;
        };
        let target = name.to_ascii_lowercase();
        let file_ok = index
            .files
            .keys()
            .all(|sibling| *sibling == target || is_83_clean(sibling));
        let dir_ok = index
            .dirs
            .keys()
            .all(|sibling| *sibling == target || is_83_clean_dir(sibling));
        file_ok && dir_ok
    }
}

pub fn normalize_rel(rel: &str) -> String {
    rel.trim_matches(['\\', '/']).replace('\\', "/")
}

pub fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

/// The guest-visible form of a relative tree path: backslashes, uppercase.
pub fn guest_path(rel: &str) -> String {
    normalize_rel(rel).replace('/', "\\").to_ascii_uppercase()
}

#[cfg(test)]
#[path = "tree_test.rs"]
pub mod tests;
