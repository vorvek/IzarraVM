// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Turn one extracted eXoDOS game plus its `dosbox.conf` into a Katea
//! `--hdd-folder` tree and the emulator invocation that runs it.
//!
//! Extraction is the caller's job (the orchestrator does it with
//! `[IO.Compression.ZipFile]`, which measures about 181 MB/s), so this works
//! against an already-extracted directory and never opens the corpus zips.
//! Nothing under the corpus root is read for write and nothing there is
//! modified.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::bat::{Flattened, Flattener};
use crate::classify::{Class, ConfVerdict, classify_conf};
use crate::conf::{AutoexecStep, DosboxConf};
use crate::recipe::Recipe;
use crate::tree::{Tree, guest_path};

/// The 15 bytes of `EXITVM.COM`, byte-identical to the copies in `duke3d_c`
/// and `quake_c`: index 0x0C on the Lotura port, exit code 0x51, command 3,
/// then halt. A game that returns to DOS ends the run instead of burning the
/// rest of the cycle budget.
pub const EXITVM_COM: [u8; 15] = [
    0xb0, 0x0c, 0xe6, 0xe4, 0xb0, 0x51, 0xe6, 0xe5, 0xb0, 0x03, 0xe6, 0xe6, 0xf4, 0xeb, 0xfd,
];

/// Which CONFIG.SYS the title gets. Keyed off the autoexec text, since the
/// `[dos]` section carries no memory intent for nine confs in ten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ConfigShape {
    /// TOKAEMM with the CD driver. Default for CD titles.
    A,
    /// TOKAEMM alone. Default for everything else.
    B,
    /// No memory manager: the title brings its own DPMI or EMM host.
    C,
    /// No memory manager, but the CD driver still has to load: 25 of the 106
    /// own-manager confs also mount a disc, and shape C would have taken their
    /// CD away along with TOKAEMM.
    D,
}

#[derive(Debug, Serialize)]
pub struct TranslateResult {
    pub short: String,
    pub class: Class,
    pub reasons: Vec<String>,
    pub flags: Vec<String>,
    pub conf: ConfVerdict,
    pub hdd_folder: PathBuf,
    pub cd_image: Option<PathBuf>,
    pub config_sys_shape: ConfigShape,
    pub autoexec: Vec<String>,
    pub launch_command: Option<String>,
    pub launch_resolved: Option<String>,
    pub resolved_by_search: bool,
    pub choices: Vec<String>,
    pub memory_mib: u32,
    pub persona: String,
    pub cycle_budget: u64,
    pub inject_keys: Option<String>,
    pub inject_mouse: Option<String>,
    pub recipe_notes: String,
    pub tree_max_depth: usize,
    pub tree_oversize_files: Vec<String>,
    pub tree_non_83_names: usize,
    /// The emulator argument vector, ready for the orchestrator to splice in
    /// the output paths.
    pub invocation: Vec<String>,
}

pub struct TranslateOptions {
    /// Where the zip was unpacked; the game directory sits inside it.
    pub extract_root: PathBuf,
    pub short: String,
    pub persona: String,
    pub clock_hz: u64,
    pub cycle_budget: u64,
    pub recipe: Recipe,
    /// Write the generated files. False makes this a dry run over the tree.
    pub write: bool,
}

/// Guest clock of each persona, for turning a recipe's guest milliseconds into
/// `--inject-keys` cycle offsets.
pub fn persona_clock_hz(persona: &str) -> u64 {
    match persona {
        "486" => 66_000_000,
        _ => 166_000_000,
    }
}

pub fn translate(
    conf: &DosboxConf,
    options: &TranslateOptions,
) -> std::io::Result<TranslateResult> {
    let verdict = classify_conf(conf);
    let mut flags: BTreeSet<String> = BTreeSet::new();
    let mut reasons: Vec<String> = verdict.reasons.clone();

    let hdd_folder = resolve_mount_root(&options.extract_root, conf, &options.short);
    let tree = Tree::index(&hdd_folder)?;
    if tree.max_depth > crate::tree::MAX_TREE_DEPTH {
        reasons.push("tree-too-deep".to_string());
    }
    if !tree.oversize_files.is_empty() {
        reasons.push("file-over-4gib".to_string());
    }
    if !tree.non_83_names.is_empty() {
        flags.insert("NON-8.3-NAMES".to_string());
    }
    // Katea reserves `DOS`, `KERNEL.SYS` and `COMMAND.COM` at the mount root
    // for the Toka-DOS overlay. A game shipping its own is folded to `~1`
    // silently, and the generated `cd \DOS` would then land in the wrong place.
    let reserved = tree.reserved_root_collisions();
    if !reserved.is_empty() {
        reasons.push("reserved-root-name".to_string());
    }

    let mut walk = walk_autoexec(conf, &tree, &mut flags);
    // The disc can be named in `[autoexec]` or inside the launcher BAT the
    // flattener just walked -- MechWarrior 2 mounts MECH2.CUE inside the CHOICE
    // branch, and reading only the conf left a 749 MB disc unmounted.
    let cd_image = resolve_cd_image(&options.extract_root, conf)
        .or_else(|| resolve_flattened_cd_image(&options.extract_root, &walk.flattened, &mut flags));
    let autoexec_lower = conf.autoexec_raw.join("\n").to_ascii_lowercase();
    // eXo sets `[dos] ems=false` on the 106 confs whose titles host their own
    // memory manager, which is the conf SAYING what the name-sniff was guessing
    // at. It is read first, and the name list stays as the fallback for the
    // confs that say nothing.
    let ems_off = conf
        .get("dos", "ems")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("false"));
    let own_manager = ems_off
        || autoexec_lower.contains("jemmex")
        || autoexec_lower.contains("jemm386")
        || autoexec_lower.contains("cwsdpmi");
    let shape = if own_manager {
        flags.insert("OWN-MEMORY-MANAGER".to_string());
        if ems_off {
            flags.insert("CONF-EMS-FALSE".to_string());
        }
        // Real mode with no memory manager never takes the V86 port path, so
        // the port-polling bucket is blind on these rows.
        flags.insert("B6-BLIND".to_string());
        if cd_image.is_some() {
            ConfigShape::D
        } else {
            ConfigShape::C
        }
    } else if cd_image.is_some() {
        ConfigShape::A
    } else {
        ConfigShape::B
    };
    if verdict.wants_gus {
        flags.insert("WANTS-GUS".to_string());
    }
    if verdict.wants_mt32 {
        flags.insert("WANTS-MT32".to_string());
    }
    if verdict.speed_sensitive {
        flags.insert("SPEED-SENSITIVE".to_string());
    }
    flags.extend(walk.flattened.flags.iter().cloned());
    if let Some(failure) = walk.flattened.failure.clone() {
        reasons.push(failure);
    }
    if walk.flattened.launch.is_none() && !walk.runs_from_cd {
        reasons.push("launch-target-unresolved".to_string());
    }
    // The `D:` prelude line is only a real instruction when a disc is actually
    // mounted there. Without `--cd-image` the guest is told to switch to a
    // drive that does not exist, the launch never resolves against the tree
    // (so there is no launch command either), and the run dies at the first
    // guest line. Refuse it here rather than emitting an AUTOEXEC that cannot
    // work: a directory-mounted CD is not something this machine can serve.
    if walk.runs_from_cd && cd_image.is_none() {
        reasons.push("cd-mount-unsupported".to_string());
    }
    // Every path component the generated AUTOEXEC names has to survive FAT
    // folding unchanged, or the guest is told to `cd` somewhere that does not
    // exist under that name. Fold fidelity is checked, never reimplemented.
    if let Some(launch) = &walk.flattened.launch {
        let mut current = String::new();
        for part in launch.dir.split('/').filter(|p| !p.is_empty()) {
            if !tree.fold_is_identity(&current, part, true) {
                flags.insert("FOLD-RISK-PATH".to_string());
            }
            current = crate::tree::join_rel(&current, part);
        }
        let file = launch.resolved.rsplit('/').next().unwrap_or_default();
        if !tree.fold_is_identity(&launch.dir, file, false) {
            flags.insert("FOLD-RISK-LAUNCH".to_string());
        }
        // The DOS PSP command tail is 127 bytes, so a long argument list is
        // silently truncated rather than refused.
        if launch.command.len() > 120 {
            flags.insert("LAUNCH-TAIL-LONG".to_string());
        }
    }

    // A mouse action the emulator would reject must fail the translation, not
    // the run: `--inject-mouse` is parsed before the machine is built, so a typo
    // would otherwise cost a whole extraction and a whole boot to discover.
    if !options.recipe.invalid_mouse_actions().is_empty() {
        reasons.push("recipe-mouse-invalid".to_string());
    }

    let class = if reasons.iter().any(|r| is_hard_reason(r)) {
        Class::Untranslatable
    } else if reasons.is_empty() {
        Class::Translatable
    } else {
        Class::Recoverable
    };

    // 61 and 63 are eXo's own DOSBox workarounds for a 64 MB machine.
    let raw_memsize = conf.memsize_mib();
    let memory_mib = if (61..=63).contains(&raw_memsize) {
        64
    } else {
        raw_memsize.clamp(4, 64)
    };
    let autoexec = render_autoexec(&walk, shape);
    // A schedule step past the budget never fires and only widens the sliced
    // region, so it is dropped rather than passed through.
    let inject_keys = options
        .recipe
        .to_inject_keys_within(options.clock_hz, options.cycle_budget);
    let inject_mouse = options
        .recipe
        .to_inject_mouse_within(options.clock_hz, options.cycle_budget);

    if options.write && class != Class::Untranslatable {
        std::fs::write(hdd_folder.join("CONFIG.SYS"), render_config_sys(shape))?;
        std::fs::write(
            hdd_folder.join("AUTOEXEC.BAT"),
            autoexec.join("\r\n") + "\r\n",
        )?;
        std::fs::write(hdd_folder.join("EXITVM.COM"), EXITVM_COM)?;
        prepare_tree(&hdd_folder)?;
    }

    let launch = walk.flattened.launch.take();
    let mut invocation = vec![
        "--cpu".to_string(),
        options.persona.clone(),
        "--memory-mib".to_string(),
        memory_mib.to_string(),
        "--video".to_string(),
        "vega".to_string(),
        "--hdd-folder".to_string(),
        hdd_folder.display().to_string(),
        "--cycles".to_string(),
        options.cycle_budget.to_string(),
    ];
    if let Some(image) = &cd_image {
        invocation.push("--cd-image".to_string());
        invocation.push(image.display().to_string());
    }
    if let Some(irq) = verdict.sb_irq.filter(|irq| *irq != 7) {
        invocation.push("--sb-irq".to_string());
        invocation.push(irq.to_string());
    }
    if let Some(keys) = &inject_keys {
        invocation.push("--inject-keys".to_string());
        invocation.push(keys.clone());
    }
    if let Some(mouse) = &inject_mouse {
        invocation.push("--inject-mouse".to_string());
        invocation.push(mouse.clone());
    }

    reasons.sort();
    reasons.dedup();
    Ok(TranslateResult {
        short: options.short.clone(),
        class,
        reasons,
        flags: flags.into_iter().collect(),
        conf: verdict,
        hdd_folder,
        cd_image,
        config_sys_shape: shape,
        autoexec,
        launch_command: launch.as_ref().map(|l| l.command.clone()),
        launch_resolved: launch.as_ref().map(|l| l.resolved.clone()),
        resolved_by_search: launch.as_ref().is_some_and(|l| l.by_search),
        choices: walk.flattened.choices.clone(),
        memory_mib,
        persona: options.persona.clone(),
        cycle_budget: options.cycle_budget,
        inject_keys,
        inject_mouse,
        recipe_notes: options.recipe.notes.clone(),
        tree_max_depth: tree.max_depth,
        tree_oversize_files: tree.oversize_files.clone(),
        tree_non_83_names: tree.non_83_names.len(),
        invocation,
    })
}

/// Reasons that mean the title is not worth launching at all. Everything else
/// is a translation the harness had to work for, which is still a run.
fn is_hard_reason(reason: &str) -> bool {
    matches!(
        reason,
        "machine-non-vga"
            | "floppy-image"
            | "booter-disk"
            | "unrecognised-image"
            | "multi-cd-swap"
            | "mount-c-count"
            | "no-launch-command"
            | "needs-basic"
            | "4dos-shell"
            | "batch-control-flow"
            | "launch-target-unresolved"
            | "call-target-unresolved"
            | "bat-backward-goto"
            | "bat-call-too-deep"
            | "bat-unreadable"
            | "bat-step-limit"
            | "tree-too-deep"
            | "reserved-root-name"
            | "file-over-4gib"
            | "cd-image-unsupported"
            | "cd-mount-unsupported"
            | "errorlevel-branch-after-program"
            | "recipe-mouse-invalid"
    )
}

struct Walk {
    /// Guest commands from the conf's own autoexec, before the launcher.
    prelude: Vec<String>,
    flattened: Flattened,
    runs_from_cd: bool,
}

fn walk_autoexec(conf: &DosboxConf, tree: &Tree, flags: &mut BTreeSet<String>) -> Walk {
    let flattener = Flattener::new(tree);
    let mut flattened = Flattened {
        vars: seed_environment(),
        ..Flattened::default()
    };
    let mut prelude: Vec<String> = Vec::new();
    let mut cwd = String::new();
    let mut drive = 'c';
    let mut seen_mount = false;
    let mut runs_from_cd = false;

    for step in &conf.autoexec {
        if flattened.launch.is_some() || flattened.failure.is_some() {
            // `exit` after the launch is the canonical ending, not a trailing
            // command; only real work left unrun is worth a flag.
            if !matches!(step, AutoexecStep::Noise | AutoexecStep::Exit) {
                flags.insert("TRAILING-COMMANDS".to_string());
            }
            continue;
        }
        match step {
            AutoexecStep::Mount { .. } => seen_mount = true,
            AutoexecStep::Drive(letter) => {
                drive = *letter;
                if *letter == 'd' {
                    runs_from_cd = true;
                    flags.insert("RUNS-FROM-CD".to_string());
                    prelude.push("D:".to_string());
                }
            }
            AutoexecStep::Cd(spec) => {
                // A `cd` before the mount is DOSBox host-side navigation.
                if !seen_mount {
                    continue;
                }
                if drive != 'c' {
                    prelude.push(format!("cd \\{}", spec.trim_start_matches(['.', '\\'])));
                    continue;
                }
                match tree.resolve_dir(&cwd, spec) {
                    Some(next) => {
                        cwd = next;
                        prelude.push(format!("cd \\{}", guest_path(&cwd)));
                    }
                    None => {
                        // The conf's `cd` naming a directory that is not there
                        // is a documented eXo shape, not an error: DOSBox
                        // prints a warning and runs the game from the root.
                        flags.insert("CONF-CD-MISSING".to_string());
                    }
                }
            }
            AutoexecStep::Call(target) => {
                if drive != 'c' {
                    prelude.push(format!("call {target}"));
                    continue;
                }
                // One resolver for `call run`, `run.bat` and a bare `border`,
                // so a conf-level CALL to a .EXE reaches the same answer a
                // BAT-level one does.
                flattener.run_line_program(&format!("call {target}\n"), &mut cwd, &mut flattened);
            }
            AutoexecStep::Command(text) => {
                let lower = text.to_ascii_lowercase();
                let verb = lower.split_whitespace().next().unwrap_or("");
                if matches!(verb, "mixer" | "ver" | "aspect" | "config")
                    || lower.starts_with("z:\\")
                {
                    continue;
                }
                if lower.starts_with("set ") || lower.starts_with("path=") {
                    prelude.push(text.clone());
                    continue;
                }
                if drive != 'c' {
                    prelude.push(text.clone());
                    continue;
                }
                let mut single = Flattened {
                    vars: flattened.vars.clone(),
                    ..Flattened::default()
                };
                let mut command_cwd = cwd.clone();
                flattener.run_line_program(
                    &format!(
                        "{text}
"
                    ),
                    &mut command_cwd,
                    &mut single,
                );
                prelude.extend(single.lines.clone());
                flattened.flags.extend(single.flags);
                flattened.choices.extend(single.choices);
                flattened.imgmounts.extend(single.imgmounts);
                flattened.vars = single.vars;
                if single.launch.is_some() {
                    flattened.launch = single.launch;
                }
                if let Some(failure) = single.failure {
                    flattened.failure = Some(failure);
                }
                cwd = command_cwd;
            }
            AutoexecStep::Pause => {
                flags.insert("PAUSE-DROPPED".to_string());
            }
            AutoexecStep::Noise | AutoexecStep::Exit | AutoexecStep::Boot => {}
            AutoexecStep::ImgMount { .. } => {}
        }
    }

    Walk {
        prelude,
        flattened,
        runs_from_cd,
    }
}

fn render_config_sys(shape: ConfigShape) -> String {
    let mut lines = vec!["FILES=40".to_string(), "LASTDRIVE=D".to_string()];
    match shape {
        ConfigShape::A => lines.push("DEVICE=C:\\DOS\\TOKAEMM.SYS RAM /T".to_string()),
        ConfigShape::B => lines.push("DEVICE=C:\\DOS\\TOKAEMM.SYS".to_string()),
        ConfigShape::C | ConfigShape::D => {}
    }
    lines.push("DOS=HIGH,UMB".to_string());
    match shape {
        ConfigShape::A => lines.push("DEVICEHIGH=C:\\DOS\\TOKACD.SYS".to_string()),
        // No memory manager means no upper memory to load high into, so the
        // driver goes in conventional memory rather than not at all.
        ConfigShape::D => lines.push("DEVICE=C:\\DOS\\TOKACD.SYS".to_string()),
        ConfigShape::B | ConfigShape::C => {}
    }
    lines.push("SHELL=C:\\DOS\\COMMAND.COM C:\\DOS /E:2048 /P=C:\\AUTOEXEC.BAT".to_string());
    lines.join("\r\n") + "\r\n"
}

/// The environment the generated AUTOEXEC has already set by the time the
/// flattened launcher runs. The walker expands `%VAR%` against this, so it has
/// to agree with `render_autoexec` line for line.
fn seed_environment() -> std::collections::BTreeMap<String, String> {
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("PATH".to_string(), "C:\\DOS".to_string());
    vars.insert("COMSPEC".to_string(), "C:\\DOS\\COMMAND.COM".to_string());
    vars.insert("BLASTER".to_string(), BLASTER_LINE.to_string());
    vars
}

const BLASTER_LINE: &str = "A220 I7 D1 H5 P300 T6";

fn render_autoexec(walk: &Walk, shape: ConfigShape) -> Vec<String> {
    let mut lines = vec![
        "@echo off".to_string(),
        "PATH C:\\DOS".to_string(),
        // `ensure_user_config` only injects a BLASTER line into an
        // emulator-stock AUTOEXEC; a generated one is user-owned and gets none.
        format!("SET BLASTER={BLASTER_LINE}"),
    ];
    // The mouse driver is loaded for every title, not only the ones known to
    // want one. Blood's launcher runs the game through `bmouse`, which aborts
    // when its INT 33h probe finds no driver, and a game that ignores INT 33h
    // pays only TOKAMOUS's residency for it.
    match shape {
        ConfigShape::A | ConfigShape::B => lines.push("LH TOKAMOUS".to_string()),
        ConfigShape::C | ConfigShape::D => lines.push("TOKAMOUS".to_string()),
    }
    if matches!(shape, ConfigShape::A | ConfigShape::D) {
        lines.push("IZCDEX /I /D:TOKACD01 /L:D /T".to_string());
    }
    lines.extend(walk.prelude.iter().cloned());
    lines.extend(walk.flattened.lines.iter().cloned());
    if let Some(launch) = &walk.flattened.launch {
        lines.push(launch.command.clone());
    }
    lines.push("C:\\EXITVM.COM".to_string());
    lines
}

/// Map a conf mount path such as `.\eXoDOS\DOOM` (or the bare parent
/// `.\eXoDOS\`, which 19% of confs use) onto the directory the zip actually
/// extracted to.
fn resolve_mount_root(extract_root: &Path, conf: &DosboxConf, short: &str) -> PathBuf {
    let mount = conf.autoexec.iter().find_map(|step| match step {
        AutoexecStep::Mount { drive: 'c', path } => Some(path.clone()),
        _ => None,
    });
    if let Some(path) = mount
        && let Some(resolved) = resolve_corpus_path(extract_root, &path)
        && resolved.is_dir()
    {
        return resolved;
    }
    let by_short = extract_root.join(short);
    if by_short.is_dir() {
        return by_short;
    }
    extract_root.to_path_buf()
}

fn resolve_cd_image(extract_root: &Path, conf: &DosboxConf) -> Option<PathBuf> {
    conf.autoexec.iter().find_map(|step| match step {
        AutoexecStep::ImgMount { image, kind, .. }
            if kind.is_empty() || kind == "cdrom" || kind == "iso" =>
        {
            // One list, shared with the census, so a conf the census counted as
            // a CD title cannot silently lose its disc here.
            if !crate::classify::is_supported_cd_extension(image) {
                return None;
            }
            resolve_corpus_path(extract_root, image).filter(|path| path.is_file())
        }
        _ => None,
    })
}

/// The first usable disc an `imgmount` inside the flattened BAT named. The path
/// is written the way the conf writes it -- relative to DOSBox's own working
/// directory, not to the guest -- so it resolves through the same mapping.
fn resolve_flattened_cd_image(
    extract_root: &Path,
    flattened: &crate::bat::Flattened,
    flags: &mut BTreeSet<String>,
) -> Option<PathBuf> {
    for mount in &flattened.imgmounts {
        if !(mount.kind.is_empty() || mount.kind == "cdrom" || mount.kind == "iso") {
            continue;
        }
        if !crate::classify::is_supported_cd_extension(&mount.image) {
            flags.insert("BAT-CD-UNSUPPORTED".to_string());
            continue;
        }
        if let Some(path) = resolve_corpus_path(extract_root, &mount.image).filter(|p| p.is_file())
        {
            flags.insert("CD-FROM-BAT".to_string());
            return Some(path);
        }
        flags.insert("BAT-CD-MISSING".to_string());
    }
    None
}

/// Strip the `.\eXoDOS\` prefix a conf path carries and walk the remainder
/// case-insensitively under the extraction root.
fn resolve_corpus_path(extract_root: &Path, spec: &str) -> Option<PathBuf> {
    let cleaned = spec.trim().trim_matches('"').replace('/', "\\");
    let mut parts: Vec<&str> = cleaned
        .split('\\')
        .map(str::trim)
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    while parts
        .first()
        .is_some_and(|p| p.eq_ignore_ascii_case("exodos"))
    {
        parts.remove(0);
    }
    let mut current = extract_root.to_path_buf();
    for part in parts {
        let mut found = None;
        for entry in std::fs::read_dir(&current).ok()? {
            let entry = entry.ok()?;
            if entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(part)
            {
                found = Some(entry.path());
                break;
            }
        }
        current = found?;
    }
    Some(current)
}

/// Make the scratch tree safe to mount: drop eXo's zero-byte `.exo` title
/// marker (the one name in a typical tree that FAT folding would rename), and
/// clear read-only bits, which make Katea's write reconciliation retry a
/// rename forever.
fn prepare_tree(root: &Path) -> std::io::Result<()> {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.file_type().is_file() && name.to_ascii_lowercase().ends_with(".exo") {
            let _ = std::fs::remove_file(path);
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            let mut perms = meta.permissions();
            if perms.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perms.set_readonly(false);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "translate_test.rs"]
mod tests;
