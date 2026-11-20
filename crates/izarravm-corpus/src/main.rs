// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

use clap::Parser;
use serde::Serialize;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Parser)]
#[command(version, about = "Scan a local DOS games corpus without modifying it.")]
struct Args {
    /// The corpus root, scanned read-only. Also read from the environment
    /// variable `IZARRAVM_CORPUS_ROOT`. The flag wins. An empty variable counts
    /// as unset. The old spelling `--dosroot` still works.
    #[arg(long, alias = "dosroot")]
    corpus_root: Option<PathBuf>,
    #[arg(long)]
    output: Option<PathBuf>,
    #[arg(long, default_value_t = 3)]
    max_depth: usize,
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CorpusScan {
    root: PathBuf,
    games: Vec<GameEntry>,
}

#[derive(Debug, Serialize)]
struct GameEntry {
    name: String,
    path: PathBuf,
    launchers: Vec<Launcher>,
}

#[derive(Debug, Serialize)]
struct Launcher {
    path: PathBuf,
    kind: LauncherKind,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
enum LauncherKind {
    Bat,
    Com,
    Exe,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let root = resolve_root(args.corpus_root, std::env::var_os("IZARRAVM_CORPUS_ROOT"))?;
    let scan = scan_corpus(&root, args.max_depth, args.limit)?;
    let json = serde_json::to_string_pretty(&scan)?;

    if let Some(output) = args.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, json)?;
    } else {
        println!("{json}");
    }

    Ok(())
}

/// The corpus root: the flag, then a non-empty environment value. There is no
/// default path, because the scanner must not guess which folder to read.
fn resolve_root(flag: Option<PathBuf>, variable: Option<OsString>) -> Result<PathBuf, String> {
    flag.or_else(|| {
        variable
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    })
    .ok_or_else(|| {
        "no corpus root: pass --corpus-root or set the environment variable IZARRAVM_CORPUS_ROOT"
            .to_string()
    })
}

fn scan_corpus(
    root: &Path,
    max_depth: usize,
    limit: Option<usize>,
) -> Result<CorpusScan, Box<dyn Error>> {
    let mut games = Vec::new();
    let mut entries = fs::read_dir(root)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries.into_iter().filter(|entry| entry.path().is_dir()) {
        if limit.is_some_and(|limit| games.len() >= limit) {
            break;
        }

        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let launchers = find_launchers(&path, max_depth);
        games.push(GameEntry {
            name,
            path,
            launchers,
        });
    }

    Ok(CorpusScan {
        root: root.to_owned(),
        games,
    })
}

fn find_launchers(game_path: &Path, max_depth: usize) -> Vec<Launcher> {
    let mut launchers = WalkDir::new(game_path)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let kind = launcher_kind(entry.path())?;
            Some(Launcher {
                path: entry.path().to_owned(),
                kind,
            })
        })
        .collect::<Vec<_>>();

    launchers.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.path.file_name().cmp(&right.path.file_name()))
    });
    launchers
}

fn launcher_kind(path: &Path) -> Option<LauncherKind> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "bat" => Some(LauncherKind::Bat),
        "com" => Some(LauncherKind::Com),
        "exe" => Some(LauncherKind::Exe),
        _ => None,
    }
}

#[cfg(test)]
#[path = "corpus_test.rs"]
mod tests;
