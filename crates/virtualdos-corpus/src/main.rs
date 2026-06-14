use clap::Parser;
use serde::Serialize;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Debug, Parser)]
#[command(version, about = "Scan a local DOS games corpus without modifying it.")]
struct Args {
    #[arg(long, env = "VIRTUALDOS_DOSROOT")]
    dosroot: Option<PathBuf>,
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
    let root = args.dosroot.unwrap_or_else(default_dosroot);
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

fn default_dosroot() -> PathBuf {
    PathBuf::from("R:\\La Colecci\u{00f3}n by Neville\\dosroot\\")
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
mod tests {
    use super::*;

    #[test]
    fn detects_dos_launchers_case_insensitively() {
        assert_eq!(
            launcher_kind(Path::new("GAME.EXE")),
            Some(LauncherKind::Exe)
        );
        assert_eq!(
            launcher_kind(Path::new("START.bat")),
            Some(LauncherKind::Bat)
        );
        assert_eq!(launcher_kind(Path::new("README.TXT")), None);
    }
}
