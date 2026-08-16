// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Per-game post-boot key schedules.
//!
//! Flattening removes the launcher menu, but it cannot get past the game's own
//! title screen, and a title screen waiting for a keypress produces a run with
//! no engine in it. So every run carries a timed key sequence: a generic one by
//! default, or a per-game recipe file when someone has worked out what a title
//! actually wants.
//!
//! Everything is expressed in GUEST milliseconds and converted to the
//! `--inject-keys` guest-cycle offsets against the persona's clock, so one
//! recipe replays identically at 486 and 586.

use serde::{Deserialize, Serialize};

/// One timed keystroke group. `text` is `--inject-keys` payload syntax: plain
/// characters, `\r` for Enter, `{space}` / `{esc}` / `{shift}` for keys with no
/// ASCII spelling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyStep {
    pub guest_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    /// Free text naming where the schedule came from.
    #[serde(default)]
    pub notes: String,
    pub keys: Vec<KeyStep>,
}

impl Recipe {
    /// The default schedule. Two jobs: clear whatever a launcher left on screen
    /// in the first few seconds (`1` then Enter is the eXo menu's own answer,
    /// kept as a safety net for the menus flattening could not resolve), then
    /// tap through a title screen.
    ///
    /// Everything lands inside the first 55 guest seconds on purpose. The
    /// classification window is the last 60 seconds of a 120-second run, and an
    /// injection schedule slices the run into one short call per scancode, so a
    /// schedule that reached into the window would put a knee inside it.
    pub fn generic() -> Recipe {
        Recipe {
            notes: "generic post-boot sequence".to_string(),
            keys: vec![
                KeyStep {
                    guest_ms: 6_000,
                    text: "1".to_string(),
                },
                KeyStep {
                    guest_ms: 7_000,
                    text: "\\r".to_string(),
                },
                KeyStep {
                    guest_ms: 12_000,
                    text: "\\r".to_string(),
                },
                KeyStep {
                    guest_ms: 20_000,
                    text: "{space}".to_string(),
                },
                KeyStep {
                    guest_ms: 30_000,
                    text: "\\r".to_string(),
                },
                KeyStep {
                    guest_ms: 40_000,
                    text: "{space}".to_string(),
                },
                KeyStep {
                    guest_ms: 50_000,
                    text: "\\r".to_string(),
                },
            ],
        }
    }

    pub fn read(path: &std::path::Path) -> Result<Recipe, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Render as an `--inject-keys` argument for a persona running at
    /// `clock_hz`. Steps are sorted and deduplicated by offset because the flag
    /// rejects a schedule whose offsets do not strictly increase.
    /// Steps past `budget_clocks` are dropped: they never fire, and they only
    /// widen the region the injection schedule slices the run into.
    pub fn to_inject_keys_within(&self, clock_hz: u64, budget_clocks: u64) -> Option<String> {
        let mut steps: Vec<(u64, &str)> = self
            .keys
            .iter()
            .map(|step| {
                (
                    (step.guest_ms.saturating_mul(clock_hz) / 1000).max(1),
                    step.text.as_str(),
                )
            })
            .collect();
        steps.sort_by_key(|(cycles, _)| *cycles);
        let mut rendered: Vec<String> = Vec::new();
        let mut last = 0u64;
        for (cycles, text) in steps {
            let cycles = cycles.max(last + 1);
            if cycles >= budget_clocks {
                break;
            }
            last = cycles;
            rendered.push(format!("{cycles}:{text}"));
        }
        if rendered.is_empty() {
            None
        } else {
            Some(rendered.join(";"))
        }
    }
}

#[cfg(test)]
#[path = "recipe_test.rs"]
mod tests;
