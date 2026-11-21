// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Pure helpers shared by `build.rs` and the crate tests.
//!
//! `build.rs` includes this file with `#[path]`. The test file
//! `tests/build_support_test.rs` includes the same file, so the logic is
//! tested without a second copy.

/// One vendored source archive.
pub struct Source {
    /// File name under `vendor/`.
    pub archive: &'static str,
    /// Top-level folder that the archive extracts to.
    pub root: &'static str,
    /// SHA-256 of the archive. Must match `vendor/SOURCES.md`.
    pub sha256: &'static str,
    /// Name of the CMake build folder (`build-<name>`), or empty if none.
    pub build: &'static str,
}

pub const SOURCES: &[Source] = &[
    Source {
        archive: "fluidsynth-2.5.6.tar.gz",
        root: "fluidsynth-2.5.6",
        sha256: "0825f024c9cf7a18073739b83612d46542ecbfb349ae9147a1e9f08e2d524407",
        build: "fluidsynth",
    },
    Source {
        archive: "libsndfile-1.2.2.tar.gz",
        root: "libsndfile-1.2.2",
        sha256: "ffe12ef8add3eaca876f04087734e6e8e029350082f3251f565fa9da55b52121",
        build: "sndfile",
    },
    Source {
        archive: "libogg-1.3.6.tar.gz",
        root: "ogg-1.3.6",
        sha256: "95b643da661155d79db9de2fca55daed3a8d491039829def246aacb3d9201c81",
        build: "ogg",
    },
    Source {
        archive: "libvorbis-1.3.7.tar.gz",
        root: "vorbis-1.3.7",
        sha256: "270c76933d0934e42c5ee0a54a36280e2d87af1de3cc3e584806357e237afd13",
        build: "vorbis",
    },
    Source {
        archive: "flac-1.4.3.tar.gz",
        root: "flac-1.4.3",
        sha256: "0a4bb82a30609b606650d538a804a7b40205366ce8fc98871b0ecf3fbb0611ee",
        build: "flac",
    },
    Source {
        archive: "opus-1.5.2.tar.gz",
        root: "opus-1.5.2",
        sha256: "9480e329e989f70d69886ded470c7f8cfe6c0667cc4196d4837ac9e668fb7404",
        build: "opus",
    },
    Source {
        archive: "gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200.tar.gz",
        root: "gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200",
        sha256: "34ab0ee87a9eb26d3087fa9b49c2572ea8ee03db0c9705b83648301a3a3fc172",
        build: "",
    },
    Source {
        archive: "munt-2.8.2.tar.gz",
        root: "munt-munt_2_8_2",
        sha256: "ca0a2b881207a88d10c511c441bcf4f7a9fd591bea630032662f02bcb51b1b5d",
        build: "munt",
    },
];

/// The CMake Visual Studio generator that matches a compiler path.
#[derive(Debug, PartialEq, Eq)]
pub struct VsGenerator {
    pub name: &'static str,
    /// Install folder that holds `VC/Tools/MSVC/`, with `/` separators.
    pub instance: Option<String>,
}

const VS_MARKER: &str = "microsoft visual studio/";
const TOOLS_MARKER: &str = "/vc/tools/msvc/";

/// Find the generator for the MSVC compiler at `compiler`. Return `None` when
/// the path shows neither the Visual Studio folder nor the toolset folder.
pub fn vs_generator(compiler: &str) -> Option<VsGenerator> {
    let path = compiler.replace('\\', "/");
    let lower = path.to_ascii_lowercase();

    let instance = lower
        .find(TOOLS_MARKER)
        .map(|index| path[..index].to_string());

    if let Some(index) = lower.find(VS_MARKER) {
        let rest = &lower[index + VS_MARKER.len()..];
        let component = rest.split('/').next().unwrap_or("");
        let name = match component {
            "18" | "2026" => Some("Visual Studio 18 2026"),
            "17" | "2022" => Some("Visual Studio 17 2022"),
            "16" | "2019" => Some("Visual Studio 16 2019"),
            _ => None,
        };
        if let Some(name) = name {
            return Some(VsGenerator { name, instance });
        }
    }

    let index = lower.find(TOOLS_MARKER)?;
    let rest = &lower[index + TOOLS_MARKER.len()..];
    let toolset = rest.split('/').next().unwrap_or("");
    let mut parts = toolset.split('.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    if major != 14 {
        return None;
    }
    let name = match minor {
        20..=29 => "Visual Studio 16 2019",
        30..=49 => "Visual Studio 17 2022",
        50..=59 => "Visual Studio 18 2026",
        _ => return None,
    };
    Some(VsGenerator { name, instance })
}

/// The first whitespace token of `cmake -E sha256sum` output, in lowercase.
/// Panics on empty output.
pub fn first_hash_token(output: &str) -> String {
    let token = output.split_whitespace().next().unwrap_or("");
    assert!(!token.is_empty(), "cmake -E sha256sum printed no hash");
    token.to_ascii_lowercase()
}
