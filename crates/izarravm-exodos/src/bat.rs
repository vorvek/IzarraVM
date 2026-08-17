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
//!
//! THE LAUNCH IS THE LAST PROGRAM ON THE PATH, not the first. A launcher
//! routinely loads a sound driver, a TSR or a logo player before the game, and
//! stopping at the first program that resolved made the driver the title's
//! launch: nine of the stage-1 sweep's 200 rows measured SOUNDRV, UNIVBE,
//! POPHINT, METASHEL, DOSJP, SETENV, SOUNDBST, PLAYLOGO or MENU and ended in
//! seconds. So the walk records every invocation and picks at the end. What
//! comes before the launch is a prelude the guest runs in order; what comes
//! after it is dropped, because it only runs once the game has exited.
//!
//! Two shapes end the walk once a program has run, and neither is more work
//! the title means to do: a backward `goto`, which is the menu loop a launcher
//! returns to when the game quits, and a label an `if errorlevel` ladder
//! named, which is another branch's body. COMMAND.COM falls into that body;
//! the launcher only reaches it because the ladder could not be read here.
//!
//! KNOWN AND DEFERRED, reviewed 2026-08-16. None of these is a wrong answer the
//! census hides; each is a shape the walker models more coarsely than
//! COMMAND.COM, and each is recorded here rather than fixed because the corpus
//! sweep has to name the frequency before the fix is worth its risk:
//!
//! - `del <directory>` prompts in real DOS ("All files in directory will be
//!   deleted!"), and only the `*.*` and bare-`*` forms are dropped here. A
//!   directory-form `del` would be emitted and would block the guest shell.
//! - `if exist` is answered against the tree as EXTRACTED, not as it stands
//!   after the flattened lines above it have run. A branch guarded on a file an
//!   earlier `copy` creates is therefore read in its pre-copy state.
//! - A flag can describe a line BELOW the launch, which is a line the guest
//!   never runs. The walk sets flags as it goes and only learns which
//!   invocation is the launch at the end, so `COMMAND-UNRESOLVED`,
//!   `MENU-KEY-INJECTED` and `IF-UNRESOLVED` over-report by a little. Measured
//!   over the stage-1 draw: 5 rows of 200, none of them with a changed
//!   AUTOEXEC.
//!
//! ENVIRONMENT. `%VAR%` is expanded from the SET lines the walk has already
//! seen, seeded with the variables the generated AUTOEXEC sets before the
//! launcher runs. A name nothing has set expands to nothing, which is what DOS
//! does and is the whole reason `XCOMUF`'s `ArchFile.Bat` picks its 16-bit
//! branch (`%PROCESSOR_ARCHITECTURE%` and `%OS%` do not exist in DOS). The one
//! way this can read a variable wrongly is a variable a PROGRAM sets, which the
//! walk cannot see; `VAR-UNSET-EXPANDED` marks every row where an unset name was
//! expanded so those rows are auditable rather than silent.
//!
//! ARGUMENTS. `%0`-`%9` come from the invocation that ran the BAT, `%0` being
//! the command that named it. An argument the caller never passed expands to
//! nothing, the way DOS reads it, and that is not what the unset-name flag
//! reports. `spellasa`'s entire launcher is one line, `Ex /dBLASTER /p220h VGA
//! /i7`, and `EX.BAT` reads the game's video mode out of `%3`: expanding the
//! numbered arguments to nothing launched the game without it.

use std::collections::{BTreeMap, BTreeSet};

use crate::tree::{Tree, guest_path, join_rel};

/// How deep a `call` chain may go before the title is refused. The autoexec's
/// own `call run` is depth 1. `SWXWCD` needs four: autoexec -> `run.bat` ->
/// `XWINGCD.BAT` -> `XWINGCD2.BAT`, and `MAX_STEPS` is what actually bounds the
/// walk, so this only has to be past the deepest real chain.
pub const MAX_CALL_DEPTH: u32 = 4;

/// How many `if errorlevel` ladders after a program invocation may be probed
/// for convergence before the title is refused outright. Each probe re-walks
/// the rest of the program once per branch, so this is a cost bound, not a
/// correctness one.
const MAX_ERRORLEVEL_PROBES: u32 = 4;

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

/// An `imgmount` the walk reached inside a BAT. `conf.rs` only sees the ones in
/// `[autoexec]`, and `MechW2` mounts its 749 MB disc inside the CHOICE branch
/// the flattener picks, so the disc has to travel out of here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgMount {
    pub drive: char,
    pub image: String,
    pub kind: String,
}

/// One program invocation the walk reached. The launch is chosen from these
/// when the walk ends; every earlier one becomes a prelude line.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub launch: Launch,
    /// How many lines the walk had emitted when this invocation ran, so the
    /// prelude line goes back in at the place it ran.
    pub prefix_len: usize,
    /// The walk's directory on this line. The launch needs a `cd` when the
    /// program lives somewhere else; a prelude never changes the directory,
    /// which is what COMMAND.COM does.
    pub cwd: String,
    /// The line to emit when this invocation turns out to be a prelude.
    pub prelude_line: String,
    /// The argument tail, lowercased, for the unload test.
    pub args: String,
}

#[derive(Debug, Default)]
pub struct Flattened {
    /// Commands to write into AUTOEXEC.BAT, in order, before the launch.
    pub lines: Vec<String>,
    pub launch: Option<Launch>,
    /// Every program invocation the walk reached, in order.
    pub candidates: Vec<Candidate>,
    /// Reason code when the BAT could not be flattened.
    pub failure: Option<String>,
    pub flags: BTreeSet<String>,
    /// One entry per `CHOICE` resolved, for the audit trail.
    pub choices: Vec<String>,
    /// The shell environment as the walk has built it. Seeded by the caller
    /// with what the generated AUTOEXEC sets before the launcher runs.
    pub vars: BTreeMap<String, String>,
    /// `imgmount` lines reached inside the BAT, in order.
    pub imgmounts: Vec<ImgMount>,
    /// How many errorlevel ladders have already been probed, so a program that
    /// is nothing but ladders cannot make the walk exponential.
    pub errorlevel_probes: u32,
}

impl Flattened {
    /// Choose the launch and settle the prelude. Every invocation before the
    /// launch is spliced back into `lines` at the place it ran, and everything
    /// after it is dropped: those lines only run once the game has exited.
    pub fn finish(&mut self) {
        if self.launch.is_some() {
            return;
        }
        let candidates = std::mem::take(&mut self.candidates);
        let Some(index) = launch_index(&candidates) else {
            return;
        };
        let mut lines = Vec::new();
        let mut from = 0;
        for candidate in &candidates[..index] {
            lines.extend_from_slice(&self.lines[from..candidate.prefix_len]);
            lines.push(candidate.prelude_line.clone());
            from = candidate.prefix_len;
        }
        let chosen = &candidates[index];
        lines.extend_from_slice(&self.lines[from..chosen.prefix_len]);
        if chosen.launch.dir != chosen.cwd {
            lines.push(format!("cd \\{}", guest_path(&chosen.launch.dir)));
        }
        self.lines = lines;
        self.launch = Some(chosen.launch.clone());
    }
}

/// Fold a nested walk into its caller. A candidate's `prefix_len` counts lines
/// in its own walk, so it moves with them.
fn absorb(out: &mut Flattened, mut nested: Flattened) {
    let offset = out.lines.len();
    out.lines.append(&mut nested.lines);
    for mut candidate in nested.candidates {
        candidate.prefix_len += offset;
        out.candidates.push(candidate);
    }
    out.flags.extend(nested.flags);
    out.choices.append(&mut nested.choices);
    out.imgmounts.append(&mut nested.imgmounts);
}

/// Which invocation is the launch: the last one that is not a teardown. The
/// first invocation is always eligible, so a path of nothing but teardown
/// names still names a launch rather than none.
fn launch_index(candidates: &[Candidate]) -> Option<usize> {
    (0..candidates.len())
        .rev()
        .find(|index| *index == 0 || !is_teardown(&candidates[*index], &candidates[..*index]))
}

/// Programs whose whole job is to undo an earlier line. `NOSOUND` (DKonK) and
/// `REMOVE` (spellasa) are the two the corpus proves; the rest are the same
/// DOS idiom. A name only counts once another program has already run, so a
/// title whose only program carries one of these names still launches.
const TEARDOWN_NAMES: [&str; 6] = [
    "nosound",
    "remove",
    "unload",
    "uninstal",
    "uninstall",
    "killtsr",
];

/// Does this invocation undo an earlier one? A TSR is usually removed by
/// running it again with a single unload switch (`Soundrv u`, `pophint /u`,
/// `metashel /K`), which is what tells it apart from a second real invocation
/// like StarFit4's `menu MEMCHK` and `MENU GO`.
fn is_teardown(candidate: &Candidate, earlier: &[Candidate]) -> bool {
    if earlier.is_empty() {
        return false;
    }
    let file = candidate
        .launch
        .resolved
        .rsplit('/')
        .next()
        .unwrap_or_default();
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    if TEARDOWN_NAMES.contains(&stem.to_ascii_lowercase().as_str()) {
        return true;
    }
    is_unload_argument(&candidate.args)
        && earlier
            .iter()
            .any(|first| first.launch.resolved == candidate.launch.resolved)
}

/// Is the whole argument tail one switch that means "unload"?
fn is_unload_argument(args: &str) -> bool {
    matches!(
        args.trim().trim_start_matches(['/', '-']),
        "u" | "k" | "r" | "x" | "off" | "unload" | "uninstall" | "kill"
    )
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

    /// Read `<dir>/<name>.BAT` from the tree and walk it. `cwd` is where the
    /// caller's shell was standing, which the BAT inherits, and `args` is what
    /// the invocation passed it. The walk only collects invocations;
    /// `Flattened::finish` chooses the launch once the outermost walk ends,
    /// because a BAT another BAT runs may hand control back and the game may
    /// sit below the return.
    fn flatten_bat(
        &self,
        cwd: &str,
        bat_rel: &str,
        depth: u32,
        args: &[String],
        out: &mut Flattened,
    ) {
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
        self.run_from(&program, 0, &mut cwd, depth, args, out);
    }

    /// Run a short synthetic program (a single autoexec payload command, most
    /// often) through the same walker, so a bare `border` and a `call run`
    /// reach one resolver. An autoexec line carries no batch arguments.
    pub fn run_line_program(&self, text: &str, cwd: &mut String, out: &mut Flattened) {
        let program = Program::parse(text);
        self.run_from(&program, 0, cwd, 1, &[], out);
        out.finish();
    }

    fn run_from(
        &self,
        program: &Program,
        start: usize,
        cwd: &mut String,
        depth: u32,
        args: &[String],
        out: &mut Flattened,
    ) {
        let mut pc = start;
        let mut steps = 0usize;
        // Labels an `if errorlevel` ladder named. Reaching one of these after a
        // program has run means the branch that was taken has ended and another
        // branch's body begins.
        let mut branch_labels: BTreeSet<String> = BTreeSet::new();
        while pc < program.lines.len() {
            steps += 1;
            if steps > MAX_STEPS {
                out.failure = Some("bat-step-limit".to_string());
                return;
            }
            let raw = program.lines[pc].clone();
            let line = expand_vars(&raw, args, out);
            let stripped = line.trim_start_matches('@').trim().to_string();
            let lower = stripped.to_ascii_lowercase();

            if stripped.is_empty() {
                pc += 1;
                continue;
            }
            if let Some(name) = stripped.strip_prefix(':') {
                let name = name
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if !out.candidates.is_empty() && branch_labels.contains(&name) {
                    return;
                }
                pc += 1;
                continue;
            }
            if lower == "exit" {
                return;
            }
            if let Some(target) = lower.strip_prefix("goto ") {
                match self.jump(program, target.trim(), pc) {
                    Some(next) => pc = next,
                    None => {
                        stop_at_backward_goto(out);
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
                            match self.jump(program, target.trim(), pc) {
                                Some(next) => pc = next,
                                None => {
                                    stop_at_backward_goto(out);
                                    return;
                                }
                            }
                            continue;
                        }
                        match self.emit(&rest, cwd, depth, out, false) {
                            Emit::Stop => return,
                            Emit::Program => {
                                if self.settle_launch(program, pc, cwd, depth, args, out)
                                    == Emit::Stop
                                {
                                    return;
                                }
                                pc += 1;
                            }
                            Emit::Continue => pc += 1,
                        }
                    }
                    IfOutcome::Unknown => {
                        out.flags.insert("IF-UNRESOLVED".to_string());
                        if let Some(label) = goto_target(&lower) {
                            branch_labels.insert(label);
                        }
                        pc += 1;
                    }
                }
                continue;
            }
            // `choice.com /c:12` and `CHOICE.EXE` are the same command as a bare
            // `choice`, and the extension has to come off before the test: a
            // fall-through here reaches `resolve_program`, which happily
            // resolves the game directory's own CHOICE.COM and records the
            // menu program as the title's launch.
            if verb_of(&lower).is_some_and(|verb| verb == "choice") {
                branch_labels.extend(ladder_labels(program, pc));
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
            match self.emit(&stripped, cwd, depth, out, false) {
                Emit::Stop => return,
                Emit::Program => {
                    if self.settle_launch(program, pc, cwd, depth, args, out) == Emit::Stop {
                        return;
                    }
                    pc += 1;
                }
                Emit::Continue => pc += 1,
            }
        }
    }

    /// A program invocation is only the launch when nothing reads its exit
    /// code. `ecstatic` runs `testmem` and then branches on `ERRORLEVEL 18` into
    /// `ecst4meg` or `ecst8meg`; taking `testmem` as the launch produces a run
    /// that measures a memory-sizing utility and then falls off the end of the
    /// AUTOEXEC. The exit code is unknowable here, so this does not guess: it
    /// walks every branch of the ladder, keeps the launch only when they all
    /// agree, and otherwise refuses the title by name.
    fn settle_launch(
        &self,
        program: &Program,
        pc: usize,
        cwd: &str,
        depth: u32,
        args: &[String],
        out: &mut Flattened,
    ) -> Emit {
        if out.failure.is_some() {
            return Emit::Stop;
        }
        let Some(ladder) = errorlevel_ladder(program, pc + 1) else {
            return Emit::Continue;
        };
        out.errorlevel_probes += 1;
        if ladder.unresolved || out.errorlevel_probes > MAX_ERRORLEVEL_PROBES {
            return refuse_errorlevel_branch(out);
        }
        let mut agreed: Option<Flattened> = None;
        for target in &ladder.continuations {
            let mut probe = Flattened {
                vars: out.vars.clone(),
                errorlevel_probes: out.errorlevel_probes,
                ..Flattened::default()
            };
            let mut probe_cwd = cwd.to_string();
            self.run_from(program, *target, &mut probe_cwd, depth, args, &mut probe);
            let reached = launch_index(&probe.candidates).map(|at| &probe.candidates[at].launch);
            let diverges = match (&agreed, reached) {
                (_, None) => true,
                (None, Some(_)) => false,
                (Some(first), Some(next)) => {
                    let at = launch_index(&first.candidates).expect("agreed reached a program");
                    let first = &first.candidates[at].launch;
                    first.command != next.command || first.dir != next.dir
                }
            };
            if diverges {
                return refuse_errorlevel_branch(out);
            }
            if agreed.is_none() {
                agreed = Some(probe);
            }
        }
        // Every branch ran the same program, so the utility above the ladder was
        // a probe and the agreed program is the launch. The utility itself still
        // has to run in the guest: it is what the game's own launcher runs.
        if let Some(probe) = agreed {
            out.flags.insert("ERRORLEVEL-BRANCH-CONVERGED".to_string());
            absorb(out, probe);
        }
        Emit::Stop
    }

    /// Jump to a label. Only forward jumps are taken: a backward `goto` is a
    /// loop, and the fixture AUTOEXECs' own `:loop` / `goto loop` shape is
    /// exactly what must never be emitted.
    fn jump(&self, program: &Program, target: &str, pc: usize) -> Option<usize> {
        Some(forward_label(program, target, pc)? + 1)
    }

    fn emit(
        &self,
        command: &str,
        cwd: &mut String,
        depth: u32,
        out: &mut Flattened,
        via_call: bool,
    ) -> Emit {
        let tokens = crate::conf::tokenize(command);
        let Some(first) = tokens.first() else {
            return Emit::Continue;
        };
        let verb = first.to_ascii_lowercase();
        // An `echo` with a redirect is not screen noise, it WRITES a file the
        // game then reads: `kq1vga` rebuilds RESOURCE.CFG out of eight of them
        // and starts in the wrong video mode without it.
        if verb.starts_with("echo") && command.contains('>') {
            out.flags.insert("ECHO-REDIRECT-KEPT".to_string());
            out.lines.push(command.trim().to_string());
            return Emit::Continue;
        }
        if is_dropped(&verb) || verb.starts_with("echo") {
            if verb == "pause" {
                out.flags.insert("PAUSE-DROPPED".to_string());
            }
            if verb == "config" {
                out.flags.insert("CONFIG-SET-DROPPED".to_string());
            }
            return Emit::Continue;
        }
        // `imgmount` is a DOSBox internal, but it is also the only place some
        // titles name their disc, so it leaves as data rather than as a line.
        if verb == "imgmount" {
            match parse_imgmount(&tokens) {
                Some(mount) => out.imgmounts.push(mount),
                None => {
                    out.flags.insert("IMGMOUNT-UNPARSED".to_string());
                }
            }
            return Emit::Continue;
        }
        if verb == "set" {
            record_set(command, out);
            out.lines.push(command.trim().to_string());
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
        // `CALL` names a BAT most of the time, but DOS lets it name any program,
        // and `ultimau1` ends its chosen branch on `call UW` -- UW.EXE, the
        // game. Treating CALL as BAT-only refused the title outright. Handing
        // the tail back to the ordinary resolver makes `call FOO` and a bare
        // `FOO` the same decision, which is what COMMAND.COM does.
        if verb == "call" && tokens.len() >= 2 {
            let Some(rest) = command.trim().split_once(char::is_whitespace) else {
                return Emit::Continue;
            };
            return self.emit(rest.1.trim(), cwd, depth, out, true);
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
            let line = force_overwrite(command.trim(), &verb, out);
            out.lines.push(line);
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
            return self.emit(&rebuilt, cwd, depth, out, via_call);
        }
        // Everything left is a program. The walk records it and carries on: the
        // launch is chosen from the whole path once the walk ends.
        match self.resolve_program(cwd, first) {
            Some((resolved, by_search, dir)) => {
                if resolved.to_ascii_lowercase().ends_with(".bat") {
                    // The child inherits the environment and hands it back: a
                    // BAT that sets a variable and returns is how XCOMUF's
                    // ArchFile.Bat decides which branch RUNXCOM.BAT takes.
                    let mut nested = Flattened {
                        vars: std::mem::take(&mut out.vars),
                        errorlevel_probes: out.errorlevel_probes,
                        ..Flattened::default()
                    };
                    // `%0` is the command that named the BAT and `%1` onwards
                    // are its arguments. spellasa's whole launcher is one line,
                    // `Ex /dBLASTER /p220h VGA /i7`, and `EX.BAT` reads the
                    // game's video mode out of `%3`.
                    let mut child_args = vec![first.trim_matches('"').to_string()];
                    child_args.extend(tokens[1..].iter().cloned());
                    self.flatten_bat(&dir, &resolved, depth + 1, &child_args, &mut nested);
                    out.vars = std::mem::take(&mut nested.vars);
                    out.errorlevel_probes = nested.errorlevel_probes;
                    let failure = nested.failure.take();
                    absorb(out, nested);
                    if let Some(failure) = failure {
                        out.failure = Some(failure);
                        return Emit::Stop;
                    }
                    // A BAT reached by CALL returns to its caller; a bare BAT
                    // invocation transfers control and never comes back. Getting
                    // this wrong stopped XCOMUF's launcher dead at the helper
                    // BAT that only sets a variable, and left MDTheif's hint TSR
                    // as the launch of the BAT that runs the game after it.
                    if via_call {
                        return Emit::Continue;
                    }
                    return Emit::Stop;
                }
                let file = resolved.rsplit('/').next().unwrap_or_default().to_string();
                let args = command
                    .trim()
                    .split_once(char::is_whitespace)
                    .map(|(_, tail)| tail.trim().to_string())
                    .unwrap_or_default();
                // A command that names its own directory must not keep that
                // prefix once the launch line has `cd`-ed into the directory:
                // DKonK's `DRIVERS\SOUNDBST` under `cd \KIDKEYS\DRIVERS` names
                // a path the guest cannot resolve.
                let launch_command = match (first.trim_matches('"').contains(['\\', '/']), &args) {
                    (false, _) => command.trim().to_string(),
                    (true, args) if args.is_empty() => file.clone(),
                    (true, args) => format!("{file} {args}"),
                };
                // A prelude does not change the directory, the way COMMAND.COM
                // does not, so a program somewhere else runs by its full path.
                let prelude_line = match (dir == *cwd, &args) {
                    (true, _) => command.trim().to_string(),
                    (false, args) if args.is_empty() => format!("\\{}", guest_path(&resolved)),
                    (false, args) => format!("\\{} {args}", guest_path(&resolved)),
                };
                out.candidates.push(Candidate {
                    prefix_len: out.lines.len(),
                    cwd: cwd.clone(),
                    prelude_line,
                    args: args.to_ascii_lowercase(),
                    launch: Launch {
                        dir,
                        command: launch_command,
                        resolved,
                        by_search,
                    },
                });
                Emit::Program
            }
            None => {
                out.flags.insert("COMMAND-UNRESOLVED".to_string());
                Emit::Continue
            }
        }
    }

    /// Resolve a command token to `(tree-relative path, by_search, directory)`.
    /// Real COMMAND.COM order is COM, EXE, BAT within a directory.
    fn resolve_program(&self, cwd: &str, token: &str) -> Option<(String, bool, String)> {
        let token = token.trim_matches('"');
        // A token that carries a path is resolved against that path alone, the
        // way DOS does: `call XcomUtil\Batch\ArchFile.Bat` must not be answered
        // by some other `ArchFile.Bat` elsewhere in the tree.
        if token.contains(['\\', '/']) {
            let cut = token.rfind(['\\', '/'])?;
            let dir = self.tree.resolve_dir(cwd, &token[..cut])?;
            let name = &token[cut + 1..];
            if name.contains('.')
                && let Some(actual) = self.tree.file_in(&dir, name)
            {
                return Some((join_rel(&dir, actual), false, dir));
            }
            for ext in ["com", "exe", "bat"] {
                if let Some(actual) = self.tree.file_in(&dir, &format!("{name}.{ext}")) {
                    return Some((join_rel(&dir, actual), false, dir.clone()));
                }
            }
            return None;
        }
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
                let rest = parts.join(" ");
                // `if exist X if not exist Y goto Z` is one line in the corpus
                // and its inner test decides the jump. Handing the whole tail
                // to `emit` would drop the inner `if` on the floor, resolve
                // nothing, and carry on down a branch the guest never takes.
                if rest.trim_start().to_ascii_lowercase().starts_with("if ") {
                    return self.eval_if(rest.trim(), cwd);
                }
                return IfOutcome::Taken(rest);
            }
            return IfOutcome::NotTaken;
        }
        // A string comparison. `%var%` was already expanded on the way in, so
        // both sides are literals and the test is decidable -- which is the
        // whole point: `Ppersia` guards its launch on `if %cheats%==no`, and an
        // undecided test dropped the only line that runs the game.
        if let Some((left, right, tail)) = split_comparison(rest) {
            if (left == right) != negated {
                if tail.trim_start().to_ascii_lowercase().starts_with("if ") {
                    return self.eval_if(tail.trim(), cwd);
                }
                return IfOutcome::Taken(tail);
            }
            return IfOutcome::NotTaken;
        }
        // `if errorlevel` outside a resolved CHOICE is unknowable here.
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
        let mut reachable: Vec<(u32, String, usize)> = Vec::new();
        for key in 1..=highest {
            if let Some((_, label)) = branches.iter().find(|(level, _)| *level <= key) {
                // Only a FORWARD branch is a branch we can take. A label above
                // the `choice` line is a menu loop: taking it walks the ladder
                // again, burns MAX_STEPS and refuses a title that runs. Such a
                // branch leaves the reachable set entirely and the rest are
                // re-scored without it, which is not the same as scoring it
                // last -- a dropped branch must never win on a tie.
                if let Some(index) = forward_label(program, label, pc) {
                    reachable.push((key, label.clone(), index));
                }
            }
        }
        if reachable.is_empty() {
            return None;
        }
        let descriptions = menu_descriptions(program, pc);
        let mut best: Option<(i32, u32, String, usize)> = None;
        for (level, label, index) in &reachable {
            let text = format!(
                "{} {}",
                label,
                descriptions.get(level).cloned().unwrap_or_default()
            );
            let score = branch_score(&text);
            let candidate = (score, *level, label.clone(), *index);
            if best
                .as_ref()
                .is_none_or(|(bs, bl, _, _)| score > *bs || (score == *bs && level < bl))
            {
                best = Some(candidate);
            }
        }
        let (score, level, label, index) = best?;
        // A menu whose every reachable branch is refused (Setup / Install /
        // Quit and nothing else) has no branch worth taking. Running the best
        // of them means running an installer over the game. Refusing here
        // sends the row to MENU-KEY-INJECTED, where a real keypress decides.
        if score <= REFUSED_BRANCH_SCORE {
            return None;
        }
        out.choices
            .push(format!("errorlevel {level} -> :{label} (score {score})"));
        Some(index + 1)
    }
}

#[derive(PartialEq, Eq)]
enum Emit {
    Continue,
    /// A program invocation was recorded. The caller still has to read whatever
    /// `if errorlevel` ladder follows it.
    Program,
    Stop,
}

/// A `goto` that points backwards is a loop. Before any program has run it is a
/// shape the walker cannot model; after one, it is the launcher returning to
/// its menu once the game exits, and the walk simply ends.
fn stop_at_backward_goto(out: &mut Flattened) {
    if out.candidates.is_empty() {
        out.failure = Some("bat-backward-goto".to_string());
    } else {
        out.flags.insert("LOOP-AFTER-LAUNCH".to_string());
    }
}

/// The exit code is unknowable here and the branches disagree about what runs,
/// so the title is refused by name rather than measured down a branch the guest
/// might never take.
fn refuse_errorlevel_branch(out: &mut Flattened) -> Emit {
    out.candidates.clear();
    out.launch = None;
    out.flags.insert("ERRORLEVEL-BRANCH".to_string());
    out.failure = Some("errorlevel-branch-after-program".to_string());
    Emit::Stop
}

enum IfOutcome {
    Taken(String),
    NotTaken,
    Unknown,
}

/// Expand `%VAR%` from the walk's environment and `%0`-`%9` from the arguments
/// the BAT was invoked with. An unset name expands to nothing, which is what
/// DOS does, and so does an argument the caller never passed.
fn expand_vars(text: &str, args: &[String], out: &mut Flattened) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut result = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '%' {
            if let Some(number) = chars.get(index + 1).and_then(|c| c.to_digit(10)) {
                if let Some(value) = args.get(number as usize) {
                    result.push_str(value);
                }
                index += 2;
                continue;
            }
            if let Some(offset) = chars[index + 1..].iter().position(|c| *c == '%') {
                let name: String = chars[index + 1..index + 1 + offset].iter().collect();
                if !name.is_empty() && !name.contains(char::is_whitespace) {
                    match out.vars.get(&name.to_ascii_uppercase()) {
                        Some(value) => result.push_str(value),
                        None => {
                            out.flags.insert("VAR-UNSET-EXPANDED".to_string());
                        }
                    }
                    index += offset + 2;
                    continue;
                }
            }
        }
        result.push(chars[index]);
        index += 1;
    }
    result
}

/// Record a `set NAME=VALUE` in the walk's environment. `set NAME=` clears it,
/// and a bare `set` (which prints the environment) changes nothing.
fn record_set(command: &str, out: &mut Flattened) {
    let Some((_, tail)) = command.trim().split_once(char::is_whitespace) else {
        return;
    };
    let Some((name, value)) = tail.split_once('=') else {
        return;
    };
    let name = name.trim().to_ascii_uppercase();
    if name.is_empty() {
        return;
    }
    if value.trim().is_empty() {
        out.vars.remove(&name);
    } else {
        out.vars.insert(name, value.trim().to_string());
    }
}

/// Split `a==b rest` (or `a == b rest`, which the corpus also writes) into its
/// two operands and whatever follows.
fn split_comparison(rest: &str) -> Option<(String, String, String)> {
    let at = rest.find("==")?;
    let left = rest[..at].trim().to_string();
    let tail = rest[at + 2..].trim_start();
    let cut = tail.find(char::is_whitespace).unwrap_or(tail.len());
    let right = tail[..cut].trim().to_string();
    Some((left, right, tail[cut..].trim().to_string()))
}

/// Give `copy` and `xcopy` an explicit overwrite switch. Without it the guest
/// stops on "Overwrite (Yes/No/All)?" and eats the injected keys that were meant
/// for the game: `SMCivili`'s whole schedule shifted about two seconds.
fn force_overwrite(command: &str, verb: &str, out: &mut Flattened) -> String {
    if !matches!(verb, "copy" | "xcopy") {
        return command.to_string();
    }
    let has_switch = crate::conf::tokenize(command)
        .iter()
        .any(|token| token.eq_ignore_ascii_case("/y") || token.eq_ignore_ascii_case("-y"));
    if has_switch {
        return command.to_string();
    }
    out.flags.insert("COPY-FORCED-OVERWRITE".to_string());
    match command.split_once(char::is_whitespace) {
        Some((head, tail)) => format!("{head} /Y {}", tail.trim_start()),
        None => command.to_string(),
    }
}

/// Read an `imgmount <drive> <image> [-t <kind>]` line. `imgmount -u d` (the
/// unmount form `Blood` opens its menu with) names no image and is not one.
fn parse_imgmount(tokens: &[String]) -> Option<ImgMount> {
    if tokens.len() < 3 || tokens[1].starts_with('-') || tokens[1].starts_with('/') {
        return None;
    }
    let kind = tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case("-t"))
        .and_then(|at| tokens.get(at + 1))
        .map(|t| t.to_ascii_lowercase())
        .unwrap_or_default();
    Some(ImgMount {
        drive: tokens[1].chars().next().unwrap_or('d').to_ascii_lowercase(),
        image: tokens[2].clone(),
        kind,
    })
}

/// The `if errorlevel N goto LABEL` ladder starting at `from`, and every place
/// control can end up after it: each forward branch target, plus the line the
/// ladder falls through to.
struct ErrorlevelLadder {
    continuations: Vec<usize>,
    /// A ladder line whose `goto` target the walker cannot follow (a backward
    /// jump, or a label that is not there). Such a branch must count as a
    /// divergence rather than quietly leaving the candidate set.
    unresolved: bool,
}

fn errorlevel_ladder(program: &Program, from: usize) -> Option<ErrorlevelLadder> {
    let mut scan = from;
    let mut targets: Vec<usize> = Vec::new();
    let mut saw_ladder = false;
    let mut unresolved = false;
    while scan < program.lines.len() {
        let line = program.lines[scan].trim().trim_start_matches('@').trim();
        let lower = line.to_ascii_lowercase();
        if line.is_empty() || lower.starts_with("rem ") || line.starts_with(':') {
            // A label between the program and its ladder would mean control can
            // arrive here from elsewhere; only blank and comment lines are skipped.
            if line.starts_with(':') {
                break;
            }
            scan += 1;
            continue;
        }
        if !lower.starts_with("if errorlevel") && !lower.starts_with("if not errorlevel") {
            break;
        }
        saw_ladder = true;
        if lower.contains(" goto ") {
            match lower
                .split_whitespace()
                .next_back()
                .and_then(|label| forward_label(program, label, scan))
            {
                Some(index) => targets.push(index + 1),
                None => unresolved = true,
            }
        } else {
            // `if errorlevel N <command>` runs a command on one branch only, so
            // the two branches cannot be compared by continuation index.
            unresolved = true;
        }
        scan += 1;
    }
    if !saw_ladder {
        return None;
    }
    targets.push(scan);
    targets.sort_unstable();
    targets.dedup();
    Some(ErrorlevelLadder {
        continuations: targets,
        unresolved,
    })
}

/// Every label the `if errorlevel N goto LABEL` ladder at `pc` names.
fn ladder_labels(program: &Program, pc: usize) -> Vec<String> {
    let mut labels = Vec::new();
    let mut scan = pc + 1;
    while scan < program.lines.len() {
        let line = program.lines[scan].trim().trim_start_matches('@').trim();
        let lower = line.to_ascii_lowercase();
        if line.is_empty() || lower.starts_with("rem ") {
            scan += 1;
            continue;
        }
        if !lower.starts_with("if errorlevel") && !lower.starts_with("if not errorlevel") {
            break;
        }
        if let Some(label) = goto_target(&lower) {
            labels.push(label);
        }
        scan += 1;
    }
    labels
}

/// The label a `... goto LABEL` line jumps to. Every ladder line ends in its
/// `goto`, so the last token is the label.
fn goto_target(lower_line: &str) -> Option<String> {
    if !lower_line.contains(" goto ") {
        return None;
    }
    lower_line
        .split_whitespace()
        .next_back()
        .map(str::to_string)
}

/// The line a label sits on, when the label exists and is strictly AFTER `pc`.
/// Only forward targets are taken: a backward one is a loop, and the fixture
/// AUTOEXECs' own `:loop` / `goto loop` shape is exactly what must never be
/// emitted.
fn forward_label(program: &Program, target: &str, pc: usize) -> Option<usize> {
    let index = *program.labels.get(&target.trim().to_ascii_lowercase())?;
    (index > pc).then_some(index)
}

/// The command verb of a line, lowercased and stripped of the executable
/// extension COMMAND.COM would have supplied.
fn verb_of(lower_line: &str) -> Option<&str> {
    let token = lower_line.split_whitespace().next()?;
    Some(
        token
            .strip_suffix(".com")
            .or_else(|| token.strip_suffix(".exe"))
            .or_else(|| token.strip_suffix(".bat"))
            .unwrap_or(token),
    )
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

/// The score of a branch that must not be taken. `resolve_choice` refuses a
/// menu whose best reachable branch scores this low rather than picking the
/// least bad destructive option.
pub const REFUSED_BRANCH_SCORE: i32 = -1000;

/// Does `text` contain `needle` as a whole word? A branch label is scored
/// alongside the menu text, and the labels are game words: `Border` (the label
/// `border` and the title `Borderwo`) contains "order", `recorder` contains it
/// too, and a substring test refuses the only branch that runs the game.
fn has_word(text: &str, needle: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric());
    let mut from = 0;
    while let Some(at) = text[from..].find(needle) {
        let start = from + at;
        let end = start + needle.len();
        if boundary(text[..start].chars().next_back()) && boundary(text[end..].chars().next()) {
            return true;
        }
        from = start + 1;
    }
    false
}

/// Rank a menu branch. We present a Sound Blaster 16 and have neither a Gravis
/// nor a Sound Canvas, so the SB branch is both the working one and the one the
/// game's own config was baked for.
pub fn branch_score(text: &str) -> i32 {
    let text = text.to_ascii_lowercase();
    // A "Game Blaster" is a Creative CMS card, not a Sound Blaster, and we
    // present neither it nor a Gravis. It has to stop matching `blaster` before
    // anything else is scored, or `Ppersia` and `kq1vga` both pick the CMS
    // branch on a tie and write CMS.DRV into the config the game then reads.
    let game_blaster = text.contains("game blaster") || text.contains("gameblaster");
    let text = text.replace("game blaster", "").replace("gameblaster", "");
    let has = |needle: &str| text.contains(needle);
    // Whole-word for the short English words, substring for the rest: `setup`
    // legitimately appears as `setup.exe` and `install` as `installer`, but
    // `order` and `help` are inside ordinary game words.
    let word = |needle: &str| has_word(&text, needle);
    if has("quit")
        || has("exit")
        || has("network")
        || has("multiplay")
        || has("setup")
        || has("install")
        || has("modem")
        || has("serial")
        || has("ipx")
        || word("order")
        || word("orders")
        || word("help")
        || has("readme")
    {
        return REFUSED_BRANCH_SCORE;
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
    if game_blaster {
        score -= 40;
    }
    // The machine is a VGA, so a menu that offers a video mode should be taken
    // at the best one it offers. Without this `kq1vga`'s CGA and EGA branches
    // tie at zero and the CGA one wins on branch order.
    //
    // WHOLE WORDS ONLY, and this one is not theoretical: a substring test reads
    // "Ghost Bear's Legacy" as an EGA branch (l-EGA-cy) and MechWarrior 2's
    // menu picks the expansion over the game.
    if word("vga") {
        score += 30;
    } else if word("ega") {
        score += 20;
    }
    if word("cga") || word("hercules") || word("monochrome") {
        score -= 20;
    }
    score
}

#[cfg(test)]
#[path = "bat_test.rs"]
mod tests;
