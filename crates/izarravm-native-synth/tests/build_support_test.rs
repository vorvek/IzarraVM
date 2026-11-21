// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Tests for the pure helpers that `build.rs` uses.

#[path = "../build_support.rs"]
#[allow(dead_code)]
mod build_support;

use build_support::{SOURCES, first_hash_token, vs_generator};

const SOURCES_MD: &str = include_str!("../vendor/SOURCES.md");

#[test]
fn every_source_hash_matches_the_sources_table() {
    for source in SOURCES {
        let row = SOURCES_MD
            .lines()
            .find(|line| line.starts_with(&format!("| `{}`", source.archive)))
            .unwrap_or_else(|| panic!("SOURCES.md has no row for {}", source.archive));
        let columns: Vec<&str> = row.split('|').map(str::trim).collect();
        // Columns: empty, archive, revision, hash, license, empty.
        assert_eq!(
            columns[3].trim_matches('`'),
            source.sha256,
            "hash of {} differs from SOURCES.md",
            source.archive
        );
    }
}

#[test]
fn standard_2022_community_path_selects_vs17() {
    let path = r"C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\cl.exe";
    let generator = vs_generator(path).unwrap();
    assert_eq!(generator.name, "Visual Studio 17 2022");
    assert_eq!(
        generator.instance.as_deref(),
        Some("C:/Program Files/Microsoft Visual Studio/2022/Community")
    );
}

#[test]
fn x86_program_files_2019_build_tools_selects_vs16() {
    let path = "C:/Program Files (x86)/Microsoft Visual Studio/2019/BuildTools/VC/Tools/MSVC/14.29.30133/bin/Hostx64/x64/cl.exe";
    let generator = vs_generator(path).unwrap();
    assert_eq!(generator.name, "Visual Studio 16 2019");
    assert_eq!(
        generator.instance.as_deref(),
        Some("C:/Program Files (x86)/Microsoft Visual Studio/2019/BuildTools")
    );
}

#[test]
fn vs_18_folder_selects_vs18() {
    let path = "C:/Program Files/Microsoft Visual Studio/18/Community/VC/Tools/MSVC/14.50.35717/bin/Hostx64/x64/cl.exe";
    assert_eq!(vs_generator(path).unwrap().name, "Visual Studio 18 2026");
}

#[test]
fn custom_build_tools_path_uses_the_toolset_folder() {
    let cases = [
        ("14.29.30133", "Visual Studio 16 2019"),
        ("14.38.33130", "Visual Studio 17 2022"),
        ("14.44.35207", "Visual Studio 17 2022"),
        ("14.51.00000", "Visual Studio 18 2026"),
    ];
    for (toolset, name) in cases {
        let path = format!("D:\\tools\\bt\\VC\\Tools\\MSVC\\{toolset}\\bin\\Hostx64\\x64\\cl.exe");
        let generator = vs_generator(&path).unwrap();
        assert_eq!(generator.name, name, "toolset {toolset}");
        assert_eq!(generator.instance.as_deref(), Some("D:/tools/bt"));
    }
}

#[test]
fn unknown_path_is_rejected() {
    assert!(vs_generator("C:/compilers/clang/bin/clang-cl.exe").is_none());
    // A year folder that is not known and no toolset folder.
    assert!(vs_generator("C:/Program Files/Microsoft Visual Studio/2015/cl.exe").is_none());
}

#[test]
fn hash_token_is_the_first_word_in_lowercase() {
    assert_eq!(first_hash_token("ABCD1234  file name.tar.gz\n"), "abcd1234");
}

#[test]
#[should_panic(expected = "no hash")]
fn empty_hash_output_panics() {
    first_hash_token("  \n");
}
