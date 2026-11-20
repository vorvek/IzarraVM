// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::tree::tests::tempdir::TempDir;

fn corpus_with_conf(dir: &TempDir) -> PathBuf {
    let game = dir.path().join("corpus").join("!dos").join("x");
    std::fs::create_dir_all(&game).unwrap();
    let conf = game.join("dosbox.conf");
    std::fs::write(&conf, "[autoexec]\n").unwrap();
    conf
}

fn conf_without_bang_dos(dir: &TempDir) -> PathBuf {
    let game = dir.path().join("games").join("x");
    std::fs::create_dir_all(&game).unwrap();
    let conf = game.join("dosbox.conf");
    std::fs::write(&conf, "").unwrap();
    conf
}

#[test]
fn a_missing_path_resolves_through_its_existing_ancestor() {
    let dir = TempDir::new();
    let resolved = canonical_lenient(&dir.path().join("a").join("b")).unwrap();
    let base = dir.path().canonicalize().unwrap();
    assert_eq!(resolved, base.join("a").join("b"));
}

/// Windows resolves `..` in a missing path before it looks at the disk, and
/// other hosts fail on it. Both outcomes are safe. A result never keeps `..`.
#[test]
fn a_parent_component_never_survives_in_a_missing_path() {
    let dir = TempDir::new();
    for tricky in [
        dir.path().join("new").join("..").join(".."),
        dir.path().join("new").join("..").join("sibling"),
    ] {
        if let Ok(resolved) = canonical_lenient(&tricky) {
            assert!(
                !resolved
                    .components()
                    .any(|component| component == Component::ParentDir)
            );
        }
    }
}

#[test]
fn a_parent_component_in_the_existing_part_is_resolved() {
    let dir = TempDir::new();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    let path = dir.path().join("a").join("..").join("fresh");
    let base = dir.path().canonicalize().unwrap();
    assert_eq!(canonical_lenient(&path).unwrap(), base.join("fresh"));
}

#[test]
fn an_extract_root_inside_the_corpus_is_refused() {
    let dir = TempDir::new();
    let conf = corpus_with_conf(&dir);
    let inside = dir.path().join("corpus").join("scratch");
    assert!(check_extract_root(&conf, &inside, None).is_err());
}

#[test]
fn an_extract_root_outside_the_corpus_is_accepted() {
    let dir = TempDir::new();
    let conf = corpus_with_conf(&dir);
    let outside = dir.path().join("scratch");
    assert!(check_extract_root(&conf, &outside, None).is_ok());
}

#[test]
fn an_extract_root_that_contains_the_corpus_is_refused() {
    let dir = TempDir::new();
    let conf = corpus_with_conf(&dir);
    assert!(check_extract_root(&conf, dir.path(), None).is_err());
}

#[test]
fn without_a_bang_dos_folder_only_the_direct_tests_apply() {
    let dir = TempDir::new();
    let conf = conf_without_bang_dos(&dir);
    let game = conf.parent().unwrap().to_path_buf();
    assert!(check_extract_root(&conf, &dir.path().join("games"), None).is_err());
    assert!(check_extract_root(&conf, &game.join("scratch"), None).is_err());
    assert!(check_extract_root(&conf, &dir.path().join("scratch"), None).is_ok());
}

#[test]
fn the_explicit_exodos_root_decides_the_corpus() {
    let dir = TempDir::new();
    let conf = conf_without_bang_dos(&dir);
    let root = dir.path().join("elsewhere");
    std::fs::create_dir_all(&root).unwrap();
    assert!(check_extract_root(&conf, &root.join("scratch"), Some(&root)).is_err());
    assert!(check_extract_root(&conf, &dir.path().join("scratch"), Some(&root)).is_ok());
}

#[test]
fn an_output_path_inside_the_corpus_is_refused() {
    let dir = TempDir::new();
    let conf = corpus_with_conf(&dir);
    let root = corpus_root(&conf, None).unwrap();
    let inside = dir.path().join("corpus").join("out").join("plan.json");
    let outside = dir.path().join("plan.json");
    assert!(check_output(&inside, root.as_deref()).is_err());
    assert!(check_output(&outside, root.as_deref()).is_ok());
}

#[test]
fn the_census_root_is_the_parent_of_the_bang_dos_folder() {
    let dir = TempDir::new();
    let dos = dir.path().join("corpus").join("!DOS");
    std::fs::create_dir_all(&dos).unwrap();
    let expected = dir.path().join("corpus").canonicalize().unwrap();
    assert_eq!(census_corpus_root(&dos).unwrap(), expected);
}
