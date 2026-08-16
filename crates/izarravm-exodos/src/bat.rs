// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Flatten an eXoDOS launcher BAT into a linear AUTOEXEC.
//!
//! 38% of the corpus autoexecs end in `call run`, and `RUN.BAT` is where the
//! launcher menu lives. `CHOICE.EXE` exists in Toka-DOS, so an unflattened
//! menu does not error out, it sits there waiting for a keypress and the run
//! looks alive while measuring nothing. The eXo template is regular enough to
//! walk directly:
//!
//! ```text
//! :start
//! if not exist *.sel goto menu
//! ... choice (change the card? Y/N)
//! :menu
//! echo Press 1 for <game> w/ Gravis Ultrasound
//! echo Press 2 for <game> w/ SoundBlaster
//! choice /C:123456 /N Please Choose:
//! if errorlevel = 2 goto SB16
//! :SB16
//! copy .\sb16\*.*
//! @GAME
//! ```
//!
//! So this walks the BAT the way COMMAND.COM would, with `if exist` answered
//! from the real extracted tree and `CHOICE` answered by preferring the branch
//! whose menu text names a Sound Blaster. What comes out has no labels, no
//! `goto` and no `choice`, which is also the cheapest thing to ask of the
//! guest shell.

use std::collections::{BTreeMap, BTreeSet};

use crate::tree::{Tree, guest_path, join_rel};

/// How deep a `call` chain may go before the title is refused. The autoexec's
/// own `call run` is depth 1; `MillAlDe` has a second-level call and is the
/// reason this is 2 rather than 1.
pub const MAX_CALL_DEPTH: u32 = 2;

/// A guard against a BAT whose control flow we mis-model. Real launchers are
/// well under a hundred steps.
const MAX_STEPS: usize = 4000;

/// The program the guest will actually run.
#[derive(Debug, Clone)]
pub struct Launch {
    /// Directory the command runs from, relative to the mounted root.
    pub dir: String,
    /// The command line as written, minus any `@`.
    pub command: String,
    /// Tree-relative path of the executable that resolved it.
    pub resolved: String,
    /// True when neither the conf's `cd` nor the BAT's cwd named the directory
    /// the executable was found in. The design's audit column.
    pub by_search: bool,
}

#[derive(Debug, Default)]
pub struct Flattened {
    /// Commands to write into AUTOEXEC.BAT, in order, before the launch.
    pub lines: Vec<String>,
    pub launch: Option<Launch>,
    /// Reason code when the BAT could not be flattened.
    pub failure: Option<String>,
    pub flags: BTreeSet<String>,
    /// One entry per `CHOICE` resolved, for the audit trail.
    pub choices: Vec<String>,
}

/// Commands the guest shell either does not have or must not be given. `pause`
/// and `choice` block; `CONFIG -set` is an eXoDOS-private command that writes
/// back into `dosbox.conf`; `mixer`, `ver` and `z:` are DOSBox internals.
fn is_dropped(verb: &str) -> bool {
    matches!(
        verb,
        "echo" | "cls" | "rem" | "pause" | "mixer" | "ver" | "config" | "@echo"
    )
}

/// Commands that are real DOS and are kept verbatim in the flattened output.
fn is_kept_builtin(verb: &str) -> bool {
    matches!(
        verb,
        "copy"
            | "del"
            | "erase"
            | "type"
            | "set"
            | "path"
            | "md"
            | "mkdir"
            | "rd"
            | "rmdir"
            | "ren"
            | "rename"
            | "attrib"
            | "move"
            | "xcopy"
    )
}

struct Program {
    lines: Vec<String>,
    labels: BTreeMap<String, usize>,
}

impl Program {
    fn parse(text: &str) -> Program {
        let lines: Vec<String> = text.lines().map(|l| l.trim().to_string()).collect();
        let mut labels = BTreeMap::new();
        for (index, line) in lines.iter().enumerate() {
            if let Some(name) = line.strip_prefix(':') {
                let name = name.split_whitespace().next().unwrap_or("").to_string();
                labels.entry(name.to_ascii_lowercase()).or_insert(index);
            }
        }
        Program { lines, labels }
    }
}

pub struct Flattener<'a> {
    tree: &'a Tree,
}

impl<'a> Flattener<'a> {
    pub fn new(tree: &'a Tree) -> Self {
        Flattener { tree }
    }

    /// Read `<dir>/<name>.BAT` from the tree and flatten it. `cwd` is where the
    /// caller's shell was standing, which the BAT inherits.
    pub fn flatten_bat(&self, cwd: &str, bat_rel: &str, depth: u32, out: &mut Flattened) {
        if depth > MAX_CALL_DEPTH {
            out.failure = Some("bat-call-too-deep".to_string());
            return;
        }
        let path = self.tree.root.join(bat_rel.replace('/', "\\"));
        let Ok(bytes) = std::fs::read(&path) else {
            out.failure = Some("bat-unreadable".to_string());
            return;
        };
        let program = Program::parse(&String::from_utf8_lossy(&bytes));
        let mut cwd = cwd.to_string();
        self.run(&program, &mut cwd, depth, out);
    }

    /// Run a short synthetic program (a single autoexec payload command, most
    /// often) through the same walker, so a bare `border` and a `call run`
    /// reach one resolver.
    pub fn run_line_program(&self, text: &str, cwd: &mut String, out: &mut Flattened) {
        let program = Program::parse(text);
        self.run(&program, cwd, 1, out);
    }

    fn run(&self, program: &Program, cwd: &mut String, depth: u32, out: &mut Flattened) {
        let mut pc = 0usize;
        let mut steps = 0usize;
        let mut jumped_to: BTreeSet<usize> = BTreeSet::new();
        while pc < program.lines.len() {
            steps += 1;
            if steps > MAX_STEPS {
                out.failure = Some("bat-step-limit".to_string());
                return;
            }
            let line = program.lines[pc].clone();
            let stripped = line.trim_start_matches('@').trim().to_string();
            let lower = stripped.to_ascii_lowercase();

            if stripped.is_empty() || stripped.starts_with(':') {
                pc += 1;
                continue;
            }
            if lower == "exit" {
                return;
            }
            if let Some(target) = lower.strip_prefix("goto ") {
                match self.jump(program, target.trim(), pc, &mut jumped_to) {
                    Some(next) => pc = next,
                    None => {
                        out.failure = Some("bat-backward-goto".to_string());
                        return;
                    }
                }
                continue;
            }
            if lower.starts_with("if ") {
                match self.eval_if(&stripped, cwd) {
                    IfOutcome::NotTaken => {
                        pc += 1;
                    }
                    IfOutcome::Taken(rest) => {
                        // Rewrite the line in place and re-run it as a plain
                        // command, which is how a taken `if exist X copy ...`
                        // and a taken `if not exist *.sel goto menu` both work.
                        let rest_lower = rest.trim().to_ascii_lowercase();
                        if let Some(target) = rest_lower.strip_prefix("goto ") {
                            match self.jump(program, target.trim(), pc, &mut jumped_to) {
                                Some(next) => pc = next,
                                None => {
                                    out.failure = Some("bat-backward-goto".to_string());
                                    return;
                                }
                            }
                            continue;
                        }
                        if self.emit(&rest, cwd, depth, out) == Emit::Stop {
                            return;
                        }
                        pc += 1;
                    }
                    IfOutcome::Unknown => {
                        out.flags.insert("IF-UNRESOLVED".to_string());
                        pc += 1;
                    }
                }
                continue;
            }
            if lower == "choice" || lower.starts_with("choice ") {
                match self.resolve_choice(program, pc, out) {
                    Some(next) => {
                        out.flags.insert("MENU-FLATTENED".to_string());
                        pc = next;
                    }
                    None => {
                        // No `if errorlevel` ladder to read, so the branch
                        // cannot be taken here. The harness key-injects instead.
                        out.flags.insert("MENU-KEY-INJECTED".to_string());
                        pc += 1;
                    }
                }
                continue;
            }
            if self.emit(&stripped, cwd, depth, out) == Emit::Stop {
                return;
            }
            pc += 1;
        }
    }

    /// Jump to a label. Only forward jumps are taken: a backward `goto` is a
    /// loop, and the fixture AUTOEXECs' own `:loop` / `goto loop` shape is
    /// exactly what must never be emitted.
    fn jump(
        &self,
        program: &Program,
        target: &str,
        pc: usize,
        jumped_to: &mut BTreeSet<usize>,
    ) -> Option<usize> {
        let index = *program.labels.get(&target.to_ascii_lowercase())?;
        if index <= pc {
            return None;
        }
        jumped_to.insert(index);
        Some(index + 1)
    }

    fn emit(&self, command: &str, cwd: &mut String, depth: u32, out: &mut Flattened) -> Emit {
        let tokens = crate::conf::tokenize(command);
        let Some(first) = tokens.first() else {
            return Emit::Continue;
        };
        let verb = first.to_ascii_lowercase();
        if is_dropped(&verb) || verb.starts_with("echo") {
            if verb == "pause" {
                out.flags.insert("PAUSE-DROPPED".to_string());
            }
            if verb == "config" {
                out.flags.insert("CONFIG-SET-DROPPED".to_string());
            }
            return Emit::Continue;
        }
        if verb == "cd" || verb == "chdir" || verb.starts_with("cd\\") || verb.starts_with("cd.") {
            let spec = if verb.len() > 2 && !verb.starts_with("chdir") {
                first[2..].to_string()
            } else {
                tokens[1..].join(" ")
            };
            match self.tree.resolve_dir(cwd, &spec) {
                Some(next) => {
                    *cwd = next;
                    out.lines.push(format!("cd \\{}", guest_path(cwd)));
                }
                None => {
                    // DOSBox prints "Directory not found" and carries on, which
                    // is exactly how the `Borderwo` recipe works by accident.
                    out.flags.insert("CD-MISSING".to_string());
                }
            }
            return Emit::Continue;
        }
        if verb == "call" && tokens.len() >= 2 {
            let target = tokens[1].trim_end_matches(".bat").trim_end_matches(".BAT");
            match self.resolve_bat(cwd, target) {
                Some(rel) => {
                    let mut nested = Flattened::default();
                    self.flatten_bat(cwd, &rel, depth + 1, &mut nested);
                    out.lines.extend(nested.lines);
                    out.flags.extend(nested.flags);
                    out.choices.extend(nested.choices);
                    if let Some(failure) = nested.failure {
                        out.failure = Some(failure);
                        return Emit::Stop;
                    }
                    if let Some(launch) = nested.launch {
                        out.launch = Some(launch);
                        return Emit::Stop;
                    }
                    return Emit::Continue;
                }
                None => {
                    out.failure = Some("call-target-unresolved".to_string());
                    return Emit::Stop;
                }
            }
        }
        if is_kept_builtin(&verb) {
            // `del *.*` is the one DOS builtin that stops and asks. A deleted
            // file never reaches the host anyway (Katea's write engine has no
            // destructive entry action), so dropping it costs nothing.
            if matches!(verb.as_str(), "del" | "erase")
                && tokens
                    .iter()
                    .skip(1)
                    .any(|arg| arg.ends_with("*.*") || arg.ends_with('*') && !arg.contains('.'))
            {
                out.flags.insert("DEL-WILDCARD-DROPPED".to_string());
                return Emit::Continue;
            }
            // FreeCOM's internal command buffer is 256 bytes.
            if command.trim().len() > 255 {
                out.flags.insert("LINE-TOO-LONG-DROPPED".to_string());
                return Emit::Continue;
            }
            out.lines.push(command.trim().to_string());
            return Emit::Continue;
        }
        // `loadfix` is a DOSBox internal that prefixes the real command.
        if verb == "loadfix" {
            out.flags.insert("LOADFIX-STRIPPED".to_string());
            let rest: Vec<&String> = tokens[1..]
                .iter()
                .filter(|t| !t.starts_with('-') && !t.starts_with('/'))
                .collect();
            if rest.is_empty() {
                return Emit::Continue;
            }
            let rebuilt = rest
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            return self.emit(&rebuilt, cwd, depth, out);
        }
        // Everything left is a program. The first one that resolves in the tree
        // is the launch, and the walk stops there.
        match self.resolve_program(cwd, first) {
            Some((resolved, by_search, dir)) => {
                if resolved.to_ascii_lowercase().ends_with(".bat") {
                    let mut nested = Flattened::default();
                    self.flatten_bat(&dir, &resolved, depth + 1, &mut nested);
                    out.lines.extend(nested.lines);
                    out.flags.extend(nested.flags);
                    out.choices.extend(nested.choices);
                    if let Some(failure) = nested.failure {
                        out.failure = Some(failure);
                    } else {
                        out.launch = nested.launch;
                    }
                    return Emit::Stop;
                }
                if dir != *cwd {
                    out.lines.push(format!("cd \\{}", guest_path(&dir)));
                    *cwd = dir.clone();
                }
                out.launch = Some(Launch {
                    dir,
                    command: command.trim().to_string(),
                    resolved,
                    by_search,
                });
                Emit::Stop
            }
            None => {
                out.flags.insert("COMMAND-UNRESOLVED".to_string());
                Emit::Continue
            }
        }
    }

    fn resolve_bat(&self, cwd: &str, name: &str) -> Option<String> {
        let candidates = self.search_dirs(cwd);
        for dir in &candidates {
            if let Some(actual) = self.tree.file_in(dir, &format!("{name}.bat")) {
                return Some(join_rel(dir, actual));
            }
        }
        None
    }

    /// Resolve a command token to `(tree-relative path, by_search, directory)`.
    /// Real COMMAND.COM order is COM, EXE, BAT within a directory.
    fn resolve_program(&self, cwd: &str, token: &str) -> Option<(String, bool, String)> {
        let token = token.trim_matches('"');
        let named_ext = token.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
        let dirs = self.search_dirs(cwd);
        for (index, dir) in dirs.iter().enumerate() {
            if named_ext.is_some()
                && let Some(actual) = self.tree.file_in(dir, token)
            {
                return Some((join_rel(dir, actual), index > 1, dir.clone()));
            }
            for ext in ["com", "exe", "bat"] {
                if let Some(actual) = self.tree.file_in(dir, &format!("{token}.{ext}")) {
                    return Some((join_rel(dir, actual), index > 1, dir.clone()));
                }
            }
        }
        None
    }

    /// cwd first, then the root, then every directory breadth-first. Index > 1
    /// is what the design calls `resolved-by-search`.
    fn search_dirs(&self, cwd: &str) -> Vec<String> {
        let mut dirs = vec![cwd.to_string()];
        if !cwd.is_empty() {
            dirs.push(String::new());
        }
        for dir in self.tree.dirs_breadth_first() {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }

    fn eval_if(&self, line: &str, cwd: &str) -> IfOutcome {
        let rest = line[2..].trim();
        let (negated, rest) = match rest.strip_prefix("not ").or(rest.strip_prefix("NOT ")) {
            Some(tail) => (true, tail.trim()),
            None => (false, rest),
        };
        let lower = rest.to_ascii_lowercase();
        if let Some(tail) = lower.strip_prefix("exist ") {
            let raw = rest[rest.len() - tail.len()..].trim();
            let mut parts = crate::conf::tokenize(raw);
            if parts.is_empty() {
                return IfOutcome::Unknown;
            }
            let target = parts.remove(0);
            let present = self.tree.exists_pattern(cwd, &target);
            if present != negated {
                return IfOutcome::Taken(parts.join(" "));
            }
            return IfOutcome::NotTaken;
        }
        // `if errorlevel` outside a resolved CHOICE, and string comparisons on
        // environment variables, are both unknowable here.
        IfOutcome::Unknown
    }

    /// Walk the `if errorlevel N goto LABEL` ladder that follows a `CHOICE`
    /// and pick a branch. Returns the line to continue from.
    fn resolve_choice(&self, program: &Program, pc: usize, out: &mut Flattened) -> Option<usize> {
        let mut branches: Vec<(u32, String)> = Vec::new();
        let mut scan = pc + 1;
        while scan < program.lines.len() {
            let line = program.lines[scan].trim().trim_start_matches('@').trim();
            if line.is_empty() || line.starts_with("rem ") {
                scan += 1;
                continue;
            }
            let lower = line.to_ascii_lowercase();
            let Some(tail) = lower.strip_prefix("if errorlevel") else {
                break;
            };
            let tail = tail.trim().trim_start_matches('=').trim();
            let level = tail
                .split_whitespace()
                .next()
                .and_then(|w| w.parse::<u32>().ok());
            // Every one of these lines ends `goto <label>`, so the last token
            // is the branch. A bare `if errorlevel N <command>` has no goto and
            // is not a branch we can take.
            let label = lower
                .split_whitespace()
                .next_back()
                .filter(|_| lower.contains(" goto "))
                .map(|s| s.to_string());
            if let (Some(level), Some(label)) = (level, label) {
                branches.push((level, label));
            }
            scan += 1;
        }
        if branches.is_empty() {
            return None;
        }
        // DOS `if errorlevel N` is `>= N`, and the ladder is evaluated in file
        // order, so the branch a keypress actually takes is the FIRST ladder
        // line whose level is at or below it. eXo writes the ladder in
        // descending order and this reduces to the obvious mapping there, but
        // an ascending ladder means every key lands on the same branch, and
        // choosing by equality would pick one the guest never reaches.
        let highest = branches.iter().map(|(level, _)| *level).max().unwrap_or(1);
        let mut reachable: Vec<(u32, String)> = Vec::new();
        for key in 1..=highest {
            if let Some((_, label)) = branches.iter().find(|(level, _)| *level <= key) {
                reachable.push((key, label.clone()));
            }
        }
        if reachable.is_empty() {
            return None;
        }
        let descriptions = menu_descriptions(program, pc);
        let mut best: Option<(i32, u32, String)> = None;
        for (level, label) in &reachable {
            let text = format!(
                "{} {}",
                label,
                descriptions.get(level).cloned().unwrap_or_default()
            );
            let score = branch_score(&text);
            let candidate = (score, *level, label.clone());
            if best
                .as_ref()
                .is_none_or(|(bs, bl, _)| score > *bs || (score == *bs && level < bl))
            {
                best = Some(candidate);
            }
        }
        let (score, level, label) = best?;
        out.choices
            .push(format!("errorlevel {level} -> :{label} (score {score})"));
        let index = *program.labels.get(&label.to_ascii_lowercase())?;
        Some(index + 1)
    }
}

#[derive(PartialEq, Eq)]
enum Emit {
    Continue,
    Stop,
}

enum IfOutcome {
    Taken(String),
    NotTaken,
    Unknown,
}

/// Read the `echo Press 2 for <game> w/ SoundBlaster` block above a `CHOICE`
/// so the branch labels can be scored against what the menu actually offers.
fn menu_descriptions(program: &Program, pc: usize) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    let start = pc.saturating_sub(40);
    for line in &program.lines[start..pc] {
        let lower = line.trim().trim_start_matches('@').to_ascii_lowercase();
        let Some(tail) = lower.strip_prefix("echo ") else {
            continue;
        };
        let tail = tail.trim();
        let Some(rest) = tail.strip_prefix("press ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(level) = words
            .next()
            .and_then(|w| w.trim_matches(',').parse::<u32>().ok())
        else {
            continue;
        };
        out.insert(level, rest.to_string());
    }
    out
}

/// Rank a menu branch. We present a Sound Blaster 16 and have neither a Gravis
/// nor a Sound Canvas, so the SB branch is both the working one and the one the
/// game's own config was baked for.
pub fn branch_score(text: &str) -> i32 {
    let text = text.to_ascii_lowercase();
    let has = |needle: &str| text.contains(needle);
    if has("quit")
        || has("exit")
        || has("network")
        || has("multiplay")
        || has("setup")
        || has("install")
        || has("modem")
        || has("serial")
        || has("ipx")
        || has("order")
        || has("help")
        || has("readme")
    {
        return -1000;
    }
    let mut score = 0;
    if has("soundblaster") || has("sound blaster") || has("sb16") || has("blaster") {
        score += 100;
    }
    if has("digital") || has("dac") {
        score += 60;
    }
    if has("adlib") || has("ad lib") || has("opl") {
        score += 40;
    }
    if has("speaker") {
        score += 20;
    }
    if has("gravis") || has("ultrasound") || has("gus") {
        score -= 40;
    }
    if has("canvas") || has("sc55") || has("roland") || has("mt-32") || has("mt32") {
        score -= 40;
    }
    score
}

#[cfg(test)]
#[path = "bat_test.rs"]
mod tests;
