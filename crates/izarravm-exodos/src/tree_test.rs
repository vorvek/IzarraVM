// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn matches_dos_wildcards_the_way_the_launcher_template_needs() {
    assert!(wildcard_match("*.sel", "sb16.sel"));
    assert!(!wildcard_match("*.sel", "default.cfg"));
    assert!(wildcard_match("*.*", "doom.exe"));
    assert!(wildcard_match("doom.*", "doom.exe"));
    assert!(!wildcard_match("doom.*", "doom2.exe"));
    assert!(wildcard_match("d??m.exe", "doom.exe"));
}

#[test]
fn recognises_names_the_fat_fold_would_leave_alone() {
    assert!(is_83_clean("DOOM.EXE"));
    assert!(is_83_clean("doom.exe"));
    assert!(is_83_clean("SETUP"));
    assert!(!is_83_clean("DOOM (1993).exo"));
    assert!(!is_83_clean("verylongname.exe"));
    assert!(!is_83_clean("archive.tar.gz"));
}

#[test]
fn a_directory_name_with_a_dot_is_not_clean() {
    // Katea treats a directory's whole name as the stem and strips dots, so
    // the guest sees `SB16CD` and a generated `cd SB16.CD` misses.
    assert!(is_83_clean("SB16.CD"));
    assert!(!is_83_clean_dir("SB16.CD"));
    assert!(is_83_clean_dir("SB16"));
}

#[test]
fn guest_paths_use_backslashes_and_uppercase() {
    assert_eq!(guest_path("games/tombraid"), "GAMES\\TOMBRAID");
    assert_eq!(guest_path(""), "");
}

fn sample_tree() -> (tempdir::TempDir, Tree) {
    let dir = tempdir::TempDir::new();
    std::fs::create_dir_all(dir.path().join("DUKE3D")).unwrap();
    std::fs::create_dir_all(dir.path().join("SB16")).unwrap();
    std::fs::write(dir.path().join("RUN.BAT"), b"@echo off\n").unwrap();
    std::fs::write(dir.path().join("DUKE3D/DUKE3D.EXE"), b"x").unwrap();
    std::fs::write(dir.path().join("Duke Nukem 3D (1996).exo"), b"").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    (dir, tree)
}

#[test]
fn the_walk_stops_at_the_depth_bound_and_reports_it() {
    // The bound is enforced INSIDE the walk: checking `max_depth` afterwards
    // means the recursion has already happened.
    let dir = tempdir::TempDir::new();
    let mut deep = dir.path().to_path_buf();
    for level in 0..(MAX_TREE_DEPTH + 8) {
        deep = deep.join(format!("D{level}"));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join("GAME.EXE"), b"x").unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    assert_eq!(tree.max_depth, MAX_TREE_DEPTH + 1);
    assert!(tree.dirs.len() <= MAX_TREE_DEPTH + 1);
}

#[test]
fn indexes_case_insensitively_and_resolves_relative_paths() {
    let (_guard, tree) = sample_tree();
    assert_eq!(tree.file_in("", "run.bat"), Some("RUN.BAT"));
    assert_eq!(tree.subdir_in("", "duke3d"), Some("DUKE3D"));
    assert_eq!(tree.resolve_dir("", ".\\DUKE3d").as_deref(), Some("DUKE3D"));
    assert_eq!(tree.resolve_dir("DUKE3D", "..").as_deref(), Some(""));
    assert_eq!(tree.resolve_dir("", "NOPE"), None);
}

#[test]
fn answers_the_sel_probe_the_launcher_opens_with() {
    let (_guard, tree) = sample_tree();
    assert!(!tree.exists_pattern("", "*.sel"));
    assert!(tree.exists_pattern("", "RUN.BAT"));
    assert!(tree.exists_pattern("DUKE3D", "duke3d.exe"));
}

#[test]
fn reports_the_exo_marker_as_the_one_unclean_name() {
    let (_guard, tree) = sample_tree();
    assert_eq!(tree.non_83_names.len(), 1);
    assert!(tree.non_83_names[0].ends_with(".exo"));
    // A single unclean sibling is enough to put the fold in play for the whole
    // directory, which is exactly why the translator deletes the marker.
    assert!(!tree.fold_is_identity("", "RUN.BAT", false));
    assert!(tree.fold_is_identity("DUKE3D", "DUKE3D.EXE", false));
}

#[test]
fn flags_a_root_level_dos_folder() {
    let dir = tempdir::TempDir::new();
    std::fs::create_dir_all(dir.path().join("DOS")).unwrap();
    let tree = Tree::index(dir.path()).unwrap();
    assert_eq!(tree.reserved_root_collisions(), vec!["dos".to_string()]);
}

/// A minimal scratch directory that removes itself. The workspace has no
/// dev-dependency on `tempfile` for this crate and one directory is not worth
/// adding it for.
pub mod tempdir {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            let id = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("izarravm-exodos-{}-{id}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("scratch directory");
            TempDir(path)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
