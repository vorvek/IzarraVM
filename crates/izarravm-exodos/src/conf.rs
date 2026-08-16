// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Parser for an eXoDOS `!dos\<short>\dosbox.conf`.
//!
//! Two jobs. The keyed sections carry the machine description (`machine=`,
//! `memsize=`, `[sblaster] irq=`) and the `[autoexec]` section carries the
//! launch recipe as raw DOSBox shell lines. Both are read leniently: two confs
//! in the corpus set the non-boolean `ems=emm386`, so nothing here parses a
//! value as a boolean, and every key is kept as its raw string.

use std::collections::BTreeMap;
use std::path::Path;

/// One `[autoexec]` line, already classified by verb. The `@` prefix DOSBox
/// uses to suppress the echo is stripped before classification because it
/// appears on every verb in the corpus (`@call`, `@mount`, `@cd`, `@imgmount`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoexecStep {
    /// `cls`, `echo ...`, `echo.`, a comment, or an empty line.
    Noise,
    /// `mount <drive> <path> [flags]`.
    Mount { drive: char, path: String },
    /// `imgmount <drive> <image> -t <kind>`.
    ImgMount {
        drive: char,
        image: String,
        kind: String,
    },
    /// A bare drive switch, `c:` or `d:`.
    Drive(char),
    /// `cd <dir>` or `cd ..`.
    Cd(String),
    /// `pause`.
    Pause,
    /// `exit`.
    Exit,
    /// `boot <image>` (a booter disk; no analogue here).
    Boot,
    /// `call <name>`: the payload is a BAT that lives inside the game tree.
    Call(String),
    /// Anything else: the launch command, or a DOSBox internal such as `mixer`.
    Command(String),
}

impl AutoexecStep {
    /// The verb, lowercased, for census histograms.
    pub fn verb(&self) -> &'static str {
        match self {
            AutoexecStep::Noise => "noise",
            AutoexecStep::Mount { .. } => "mount",
            AutoexecStep::ImgMount { .. } => "imgmount",
            AutoexecStep::Drive(_) => "drive",
            AutoexecStep::Cd(_) => "cd",
            AutoexecStep::Pause => "pause",
            AutoexecStep::Exit => "exit",
            AutoexecStep::Boot => "boot",
            AutoexecStep::Call(_) => "call",
            AutoexecStep::Command(_) => "command",
        }
    }
}

/// A parsed `dosbox.conf`.
#[derive(Debug, Clone, Default)]
pub struct DosboxConf {
    /// `section` -> `key` -> raw value, both lowercased for the lookup.
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
    /// Every `[autoexec]` line as written, minus trailing whitespace.
    pub autoexec_raw: Vec<String>,
    /// The same lines, classified.
    pub autoexec: Vec<AutoexecStep>,
}

impl DosboxConf {
    pub fn parse(text: &str) -> Self {
        let mut conf = DosboxConf::default();
        let mut section = String::new();
        for line in text.lines() {
            let trimmed = line.trim();
            if let Some(name) = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                section = name.trim().to_ascii_lowercase();
                continue;
            }
            if section == "autoexec" {
                conf.autoexec_raw.push(trimmed.to_string());
                conf.autoexec.push(parse_autoexec_line(trimmed));
                continue;
            }
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let Some((key, value)) = trimmed.split_once('=') else {
                continue;
            };
            conf.sections
                .entry(section.clone())
                .or_default()
                .insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
        conf
    }

    pub fn read(path: &Path) -> std::io::Result<Self> {
        // eXo writes these as plain ASCII, but a handful carry stray high bytes
        // in an echo line, so decode lossily rather than failing the whole conf.
        let bytes = std::fs::read(path)?;
        Ok(DosboxConf::parse(&String::from_utf8_lossy(&bytes)))
    }

    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections.get(section)?.get(key).map(String::as_str)
    }

    /// `[dosbox] machine=`, lowercased.
    pub fn machine(&self) -> &str {
        self.get("dosbox", "machine").unwrap_or("").trim()
    }

    /// `[dosbox] memsize=` in MiB. Absent reads as DOSBox's own default of 16.
    pub fn memsize_mib(&self) -> u32 {
        self.get("dosbox", "memsize")
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or(16)
    }

    /// `[cpu] cycles=` as written: `auto`, `max`, a number, or `fixed N`.
    pub fn cycles(&self) -> &str {
        self.get("cpu", "cycles").unwrap_or("").trim()
    }

    /// The numeric part of `cycles=`, when there is one. `auto` and `max` have none.
    pub fn cycles_fixed(&self) -> Option<u64> {
        let raw = self.cycles().to_ascii_lowercase();
        let tail = raw.strip_prefix("fixed").unwrap_or(&raw).trim();
        tail.split_whitespace().next()?.parse::<u64>().ok()
    }

    pub fn sb_irq(&self) -> Option<u8> {
        self.get("sblaster", "irq")?.trim().parse::<u8>().ok()
    }

    pub fn wants_gus(&self) -> bool {
        self.get("gus", "gus")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
    }

    pub fn wants_mt32(&self) -> bool {
        self.get("midi", "mididevice")
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("mt32"))
    }
}

/// Split a DOSBox shell line into tokens, honouring double quotes. `XCOMUF`
/// and several other CD titles quote the image path because the ISO name
/// contains spaces, so an unquoted split loses the argument.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut have = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            c if c.is_whitespace() && !quoted => {
                if have {
                    out.push(std::mem::take(&mut current));
                    have = false;
                }
            }
            c => {
                current.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(current);
    }
    out
}

fn parse_autoexec_line(raw: &str) -> AutoexecStep {
    let line = raw.trim().trim_start_matches('@').trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with("rem ") {
        return AutoexecStep::Noise;
    }
    let lower = line.to_ascii_lowercase();
    if lower == "cls" || lower == "echo." || lower.starts_with("echo ") || lower == "echo" {
        return AutoexecStep::Noise;
    }
    if lower == "pause" {
        return AutoexecStep::Pause;
    }
    if lower == "exit" {
        return AutoexecStep::Exit;
    }
    // `c:` / `d:` / `a:`.
    if line.len() == 2 && line.ends_with(':') && line.as_bytes()[0].is_ascii_alphabetic() {
        return AutoexecStep::Drive(line.as_bytes()[0].to_ascii_lowercase() as char);
    }
    let tokens = tokenize(line);
    let Some(verb) = tokens.first().map(|t| t.to_ascii_lowercase()) else {
        return AutoexecStep::Noise;
    };
    match verb.as_str() {
        "mount" if tokens.len() >= 3 => AutoexecStep::Mount {
            drive: drive_letter(&tokens[1]),
            path: tokens[2].clone(),
        },
        "imgmount" if tokens.len() >= 3 => {
            let kind = tokens
                .iter()
                .position(|t| t.eq_ignore_ascii_case("-t"))
                .and_then(|i| tokens.get(i + 1))
                .map(|t| t.to_ascii_lowercase())
                .unwrap_or_default();
            AutoexecStep::ImgMount {
                drive: drive_letter(&tokens[1]),
                image: tokens[2].clone(),
                kind,
            }
        }
        "cd" | "chdir" if tokens.len() >= 2 => AutoexecStep::Cd(tokens[1..].join(" ")),
        "boot" => AutoexecStep::Boot,
        "call" if tokens.len() >= 2 => AutoexecStep::Call(tokens[1..].join(" ")),
        _ => AutoexecStep::Command(line.to_string()),
    }
}

fn drive_letter(token: &str) -> char {
    token
        .chars()
        .next()
        .unwrap_or('c')
        .to_ascii_lowercase()
        .to_owned()
}

#[cfg(test)]
#[path = "conf_test.rs"]
mod tests;
