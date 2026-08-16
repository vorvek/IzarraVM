// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

//! eXoDOS corpus tooling: classify the collection's `dosbox.conf` files, and
//! translate one extracted game into a Katea `--hdd-folder` tree plus the
//! emulator invocation that runs it.
//!
//! The corpus is read-only. Nothing here opens a path under the corpus root
//! for write, and the translation always works on a scratch copy the caller
//! extracted.

mod bat;
mod classify;
mod conf;
mod recipe;
mod translate;
mod tree;

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::classify::{Class, classify_conf};
use crate::conf::DosboxConf;
use crate::recipe::Recipe;
use crate::translate::{TranslateOptions, persona_clock_hz, translate};

#[derive(Debug, Parser)]
#[command(version, about = "Classify and translate the eXoDOS corpus.")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Classify every `<dos-root>/<short>/dosbox.conf` and report the census.
    Census {
        /// The corpus `!dos` directory.
        #[arg(long)]
        dos_root: PathBuf,
        /// Directory for `census.jsonl`, `census.tsv` and `census-summary.json`.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Translate one extracted game into a runnable folder plus an invocation.
    Translate {
        /// The game's `dosbox.conf`, normally `<dos-root>/<short>/dosbox.conf`.
        #[arg(long)]
        conf: PathBuf,
        /// Where the zip was unpacked. The game directory sits inside it.
        #[arg(long)]
        extract_root: PathBuf,
        /// The corpus short name, used to find the game directory and to name
        /// the recipe file.
        #[arg(long)]
        short: String,
        #[arg(long, default_value = "586")]
        persona: String,
        /// Guest-clock budget. The default is 120 guest seconds at 586.
        #[arg(long, default_value_t = 20_000_000_000)]
        cycles: u64,
        /// A key-injection recipe. Overrides `--recipe-dir`.
        #[arg(long)]
        recipe: Option<PathBuf>,
        /// Directory searched for `<short>.json`; the generic schedule is used
        /// when there is no per-game file.
        #[arg(long)]
        recipe_dir: Option<PathBuf>,
        /// Do not write CONFIG.SYS, AUTOEXEC.BAT or EXITVM.COM.
        #[arg(long)]
        dry_run: bool,
        /// Write the translation record here as well as to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Print the built-in generic key schedule, as a recipe-file template.
    DefaultRecipe,
}

fn main() -> Result<(), Box<dyn Error>> {
    match Args::parse().command {
        Command::Census { dos_root, output } => run_census(&dos_root, output.as_deref()),
        Command::Translate {
            conf,
            extract_root,
            short,
            persona,
            cycles,
            recipe,
            recipe_dir,
            dry_run,
            output,
        } => {
            let recipe = load_recipe(recipe.as_deref(), recipe_dir.as_deref(), &short)?;
            let parsed = DosboxConf::read(&conf)?;
            let options = TranslateOptions {
                extract_root,
                short,
                clock_hz: persona_clock_hz(&persona),
                persona,
                cycle_budget: cycles,
                recipe,
                write: !dry_run,
            };
            let result = translate(&parsed, &options)?;
            let json = serde_json::to_string_pretty(&result)?;
            if let Some(path) = output {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(path, &json)?;
            }
            println!("{json}");
            Ok(())
        }
        Command::DefaultRecipe => {
            println!("{}", serde_json::to_string_pretty(&Recipe::generic())?);
            Ok(())
        }
    }
}

fn load_recipe(
    explicit: Option<&Path>,
    dir: Option<&Path>,
    short: &str,
) -> Result<Recipe, Box<dyn Error>> {
    if let Some(path) = explicit {
        return Ok(Recipe::read(path)?);
    }
    if let Some(dir) = dir {
        let path = dir.join(format!("{short}.json"));
        if path.is_file() {
            return Ok(Recipe::read(&path)?);
        }
    }
    Ok(Recipe::generic())
}

#[derive(Debug, Serialize)]
struct CensusSummary {
    confs: usize,
    translatable: usize,
    recoverable: usize,
    untranslatable: usize,
    classifiable_share: f64,
    reason_histogram: BTreeMap<String, usize>,
    machine_histogram: BTreeMap<String, usize>,
    with_call: usize,
    with_cd_image: usize,
    speed_sensitive: usize,
    wants_gus: usize,
    wants_mt32: usize,
    sb_irq_histogram: BTreeMap<String, usize>,
    autoexec_verb_histogram: BTreeMap<String, usize>,
}

fn run_census(dos_root: &Path, output: Option<&Path>) -> Result<(), Box<dyn Error>> {
    let mut rows = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dos_root)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    entries.sort();

    let mut summary = CensusSummary {
        confs: 0,
        translatable: 0,
        recoverable: 0,
        untranslatable: 0,
        classifiable_share: 0.0,
        reason_histogram: BTreeMap::new(),
        machine_histogram: BTreeMap::new(),
        with_call: 0,
        with_cd_image: 0,
        speed_sensitive: 0,
        wants_gus: 0,
        wants_mt32: 0,
        sb_irq_histogram: BTreeMap::new(),
        autoexec_verb_histogram: BTreeMap::new(),
    };

    for dir in entries {
        let conf_path = dir.join("dosbox.conf");
        if !conf_path.is_file() {
            continue;
        }
        let short = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let conf = DosboxConf::read(&conf_path)?;
        let verdict = classify_conf(&conf);
        summary.confs += 1;
        match verdict.class {
            Class::Translatable => summary.translatable += 1,
            Class::Recoverable => summary.recoverable += 1,
            Class::Untranslatable => summary.untranslatable += 1,
        }
        for reason in &verdict.reasons {
            *summary.reason_histogram.entry(reason.clone()).or_insert(0) += 1;
        }
        let machine = if verdict.machine.is_empty() {
            "(unset)".to_string()
        } else {
            verdict.machine.clone()
        };
        *summary.machine_histogram.entry(machine).or_insert(0) += 1;
        summary.with_call += usize::from(verdict.has_call);
        summary.with_cd_image += usize::from(verdict.cd_image.is_some());
        summary.speed_sensitive += usize::from(verdict.speed_sensitive);
        summary.wants_gus += usize::from(verdict.wants_gus);
        summary.wants_mt32 += usize::from(verdict.wants_mt32);
        *summary
            .sb_irq_histogram
            .entry(
                verdict
                    .sb_irq
                    .map(|irq| irq.to_string())
                    .unwrap_or_else(|| "(unset)".to_string()),
            )
            .or_insert(0) += 1;
        for step in &conf.autoexec {
            *summary
                .autoexec_verb_histogram
                .entry(step.verb().to_string())
                .or_insert(0) += 1;
        }
        rows.push((short, verdict));
    }

    summary.classifiable_share = if summary.confs == 0 {
        0.0
    } else {
        (summary.translatable + summary.recoverable) as f64 / summary.confs as f64
    };

    if let Some(dir) = output {
        std::fs::create_dir_all(dir)?;
        let mut jsonl = String::new();
        let mut tsv = String::from(
            "short\tclass\treasons\tmachine\tmemsize\tcycles\tsb_irq\tgus\tmt32\tcd_image\tcall\tpayload\n",
        );
        for (short, verdict) in &rows {
            jsonl.push_str(&serde_json::to_string(&serde_json::json!({
                "short": short,
                "verdict": verdict,
            }))?);
            jsonl.push('\n');
            tsv.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                short,
                verdict.class.as_str(),
                verdict.reasons.join("|"),
                verdict.machine,
                verdict.memsize_mib,
                verdict.cycles,
                verdict
                    .sb_irq
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| String::from("")),
                verdict.wants_gus,
                verdict.wants_mt32,
                verdict.cd_image.clone().unwrap_or_default(),
                verdict.has_call,
                verdict.payload_commands,
            ));
        }
        std::fs::write(dir.join("census.jsonl"), jsonl)?;
        std::fs::write(dir.join("census.tsv"), tsv)?;
        std::fs::write(
            dir.join("census-summary.json"),
            serde_json::to_string_pretty(&summary)?,
        )?;
    }

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
