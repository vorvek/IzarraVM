// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCES: &[(&str, &str)] = &[
    ("fluidsynth-2.5.6.tar.gz", "fluidsynth-2.5.6"),
    ("libsndfile-1.2.2.tar.gz", "libsndfile-1.2.2"),
    ("libogg-1.3.6.tar.gz", "ogg-1.3.6"),
    ("libvorbis-1.3.7.tar.gz", "vorbis-1.3.7"),
    ("flac-1.4.3.tar.gz", "flac-1.4.3"),
    ("opus-1.5.2.tar.gz", "opus-1.5.2"),
    (
        "gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200.tar.gz",
        "gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200",
    ),
    ("munt-2.8.2.tar.gz", "munt-munt_2_8_2"),
];

fn main() {
    let target = env::var("TARGET").expect("Cargo provides TARGET");
    let host = env::var("HOST").expect("Cargo provides HOST");
    println!("cargo::rustc-check-cfg=cfg(izarravm_native_synth_unavailable)");
    let supported = target == host
        && matches!(
            target.as_str(),
            "x86_64-pc-windows-msvc" | "x86_64-unknown-linux-gnu"
        );
    if !supported {
        println!("cargo::rustc-cfg=izarravm_native_synth_unavailable");
        return;
    }

    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let vendor_dir = crate_dir.join("vendor");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("native");
    let source_dir = out_dir.join("source");
    fs::create_dir_all(&source_dir).unwrap();

    for (archive, root) in SOURCES {
        println!("cargo::rerun-if-changed=vendor/{archive}");
        extract(&vendor_dir.join(archive), &source_dir, root);
    }
    let fluid_gcem = source_dir.join("fluidsynth-2.5.6").join("gcem");
    if !fluid_gcem.join("include").join("gcem.hpp").is_file() {
        run(
            Command::new("cmake")
                .args(["-E", "copy_directory"])
                .arg(source_dir.join("gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200"))
                .arg(&fluid_gcem),
            "populate FluidSynth's pinned gcem submodule",
        );
    }
    assert_offline_sources(&source_dir);

    let install_dir = out_dir.join("install");
    build_project(
        "ogg",
        &source_dir.join("ogg-1.3.6"),
        &out_dir,
        &install_dir,
        "install",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("BUILD_TESTING", "OFF"),
            ("INSTALL_DOCS", "OFF"),
            ("INSTALL_PKG_CONFIG_MODULE", "OFF"),
        ],
    );
    build_project(
        "vorbis",
        &source_dir.join("vorbis-1.3.7"),
        &out_dir,
        &install_dir,
        "install",
        &[("BUILD_SHARED_LIBS", "OFF"), ("BUILD_TESTING", "OFF")],
    );
    build_project(
        "flac",
        &source_dir.join("flac-1.4.3"),
        &out_dir,
        &install_dir,
        "install",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("BUILD_CXXLIBS", "OFF"),
            ("BUILD_PROGRAMS", "OFF"),
            ("BUILD_EXAMPLES", "OFF"),
            ("BUILD_TESTING", "OFF"),
            ("BUILD_DOCS", "OFF"),
            ("INSTALL_MANPAGES", "OFF"),
            ("INSTALL_PKGCONFIG_MODULES", "OFF"),
        ],
    );
    build_project(
        "opus",
        &source_dir.join("opus-1.5.2"),
        &out_dir,
        &install_dir,
        "install",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("BUILD_TESTING", "OFF"),
            ("OPUS_BUILD_PROGRAMS", "OFF"),
            ("OPUS_BUILD_TESTING", "OFF"),
            ("OPUS_INSTALL_PKG_CONFIG_MODULE", "OFF"),
        ],
    );
    build_project(
        "sndfile",
        &source_dir.join("libsndfile-1.2.2"),
        &out_dir,
        &install_dir,
        "install",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("BUILD_TESTING", "OFF"),
            ("BUILD_PROGRAMS", "OFF"),
            ("BUILD_EXAMPLES", "OFF"),
            ("BUILD_REGTEST", "OFF"),
            ("ENABLE_CPACK", "OFF"),
            ("ENABLE_EXTERNAL_LIBS", "ON"),
            ("ENABLE_MPEG", "OFF"),
            ("ENABLE_EXPERIMENTAL", "OFF"),
            ("INSTALL_MANPAGES", "OFF"),
            ("CMAKE_DISABLE_FIND_PACKAGE_mp3lame", "ON"),
            ("CMAKE_DISABLE_FIND_PACKAGE_mpg123", "ON"),
            ("CMAKE_DISABLE_FIND_PACKAGE_Speex", "ON"),
            ("CMAKE_DISABLE_FIND_PACKAGE_SQLite3", "ON"),
        ],
    );
    let fluid_build = build_project(
        "fluidsynth",
        &source_dir.join("fluidsynth-2.5.6"),
        &out_dir,
        &install_dir,
        "libfluidsynth",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("osal", "cpp11"),
            ("enable-libsndfile", "ON"),
            ("enable-libinstpatch", "OFF"),
            ("enable-alsa", "OFF"),
            ("enable-aufile", "OFF"),
            ("enable-dbus", "OFF"),
            ("enable-dsound", "OFF"),
            ("enable-ipv6", "OFF"),
            ("enable-jack", "OFF"),
            ("enable-ladspa", "OFF"),
            ("enable-midishare", "OFF"),
            ("enable-native-dls", "OFF"),
            ("enable-network", "OFF"),
            ("enable-openmp", "OFF"),
            ("enable-oss", "OFF"),
            ("enable-pipewire", "OFF"),
            ("enable-pulseaudio", "OFF"),
            ("enable-readline", "OFF"),
            ("enable-sdl3", "OFF"),
            ("enable-wasapi", "OFF"),
            ("enable-waveout", "OFF"),
            ("enable-winmidi", "OFF"),
        ],
    );
    let munt_build = build_project(
        "munt",
        &source_dir.join("munt-munt_2_8_2"),
        &out_dir,
        &install_dir,
        "mt32emu",
        &[
            ("BUILD_SHARED_LIBS", "OFF"),
            ("BUILD_TESTING", "OFF"),
            ("munt_WITH_MT32EMU_QT", "OFF"),
            ("munt_WITH_MT32EMU_SMF2WAV", "OFF"),
            ("munt_WITH_MT32EMU_WIN32DRV", "OFF"),
            ("libmt32emu_C_INTERFACE", "ON"),
            ("libmt32emu_SHARED", "OFF"),
        ],
    );

    for dir in [
        install_dir.join("lib"),
        fluid_build.join("src"),
        fluid_build.join("src").join("Release"),
        munt_build.join("mt32emu"),
        munt_build.join("mt32emu").join("Release"),
    ] {
        println!("cargo::rustc-link-search=native={}", dir.display());
    }

    if target.contains("windows") {
        for library in [
            "libfluidsynth-3",
            "sndfile",
            "FLAC",
            "opus",
            "vorbisenc",
            "vorbisfile",
            "vorbis",
            "ogg",
            "mt32emu",
        ] {
            println!("cargo::rustc-link-lib=static={library}");
        }
    } else {
        for library in [
            "fluidsynth",
            "sndfile",
            "FLAC",
            "opus",
            "vorbisenc",
            "vorbisfile",
            "vorbis",
            "ogg",
            "mt32emu",
        ] {
            println!("cargo::rustc-link-lib=static={library}");
        }
        for library in ["stdc++", "m", "pthread", "dl"] {
            println!("cargo::rustc-link-lib={library}");
        }
    }
}

fn extract(archive: &Path, destination: &Path, root: &str) {
    if destination.join(root).exists() {
        return;
    }
    run(
        Command::new("cmake")
            .args(["-E", "tar", "xzf"])
            .arg(archive)
            .current_dir(destination),
        "extract native source archive",
    );
}

fn build_project(
    name: &str,
    source: &Path,
    out_dir: &Path,
    install_dir: &Path,
    target: &str,
    definitions: &[(&str, &str)],
) -> PathBuf {
    let build = out_dir.join(format!("build-{name}"));
    fs::create_dir_all(&build).unwrap();
    let prefix = install_dir.to_string_lossy().replace('\\', "/");

    let mut configure = Command::new("cmake");
    configure
        .args(["-S"])
        .arg(source)
        .args(["-B"])
        .arg(&build)
        .args(cmake_generator())
        .arg("-DCMAKE_BUILD_TYPE=Release")
        .arg("-DCMAKE_POLICY_VERSION_MINIMUM=3.5")
        .arg("-DCMAKE_FIND_PACKAGE_PREFER_CONFIG=ON")
        .arg("-DFETCHCONTENT_FULLY_DISCONNECTED=ON")
        .arg("-DFETCHCONTENT_UPDATES_DISCONNECTED=ON")
        .arg(format!("-DCMAKE_INSTALL_PREFIX={prefix}"))
        .arg(format!("-DCMAKE_PREFIX_PATH={prefix}"))
        .arg("-DCMAKE_INSTALL_LIBDIR=lib");
    for (key, value) in definitions {
        configure.arg(format!("-D{key}={value}"));
    }
    compiler_environment(&mut configure);
    run(&mut configure, &format!("configure {name}"));

    let mut compile = Command::new("cmake");
    compile.args(["--build"]).arg(&build).args([
        "--config",
        "Release",
        "--target",
        target,
        "--parallel",
    ]);
    compiler_environment(&mut compile);
    run(&mut compile, &format!("build {name}"));
    build
}

fn assert_offline_sources(source_dir: &Path) {
    let allowed = source_dir
        .join("fluidsynth-2.5.6")
        .join("cmake_admin")
        .join("FindGCEM.cmake");
    let mut pending = vec![source_dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("cmake")
            ) || path.file_name() == Some(std::ffi::OsStr::new("CMakeLists.txt"))
            {
                let contents = fs::read_to_string(&path).unwrap();
                let has_unapproved_fetch = contents.contains("ExternalProject_Add")
                    || contents.contains("FetchContent_Declare")
                    || contents.contains("GIT_REPOSITORY");
                assert!(
                    !has_unapproved_fetch
                        && (!contents.contains("file(DOWNLOAD") || path == allowed),
                    "native build source contains an unapproved download: {}",
                    path.display()
                );
            }
        }
    }
}

fn cmake_generator() -> Vec<String> {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Vec::new();
    }

    let mut build = cc::Build::new();
    build.cargo_metadata(false);
    let compiler = build
        .get_compiler()
        .path()
        .to_string_lossy()
        .replace('\\', "/");
    let generator = if compiler.contains("/18/") || compiler.contains("/2026/") {
        "Visual Studio 18 2026"
    } else if compiler.contains("/17/") || compiler.contains("/2022/") {
        "Visual Studio 17 2022"
    } else if compiler.contains("/16/") || compiler.contains("/2019/") {
        "Visual Studio 16 2019"
    } else {
        panic!("unsupported MSVC installation: {compiler}");
    };
    vec!["-G".into(), generator.into(), "-A".into(), "x64".into()]
}

fn compiler_environment(command: &mut Command) {
    let mut c_build = cc::Build::new();
    c_build.cargo_metadata(false);
    let c = c_build.get_compiler();
    let mut cpp_build = cc::Build::new();
    cpp_build.cpp(true).cargo_metadata(false);
    let cpp = cpp_build.get_compiler();
    command.env("CC", c.path()).env("CXX", cpp.path());
    for (key, value) in c.env().iter().chain(cpp.env()) {
        command.env(key, value);
    }
}

fn run(command: &mut Command, action: &str) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command.status().unwrap_or_else(|error| {
        panic!("failed to {action}: {error}");
    });
    assert!(status.success(), "failed to {action}: {status}");
}
