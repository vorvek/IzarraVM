// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Structural-bucket classification of one archived sweep row.
//!
//! The orchestrator (`scripts/sweep-exodos.ps1`) collects; this decides. It
//! turns an `izarravm-hdd-profile-v1` JSON, its phase-mark series and its
//! screen index into an outcome, a set of bucket flags with the measured
//! metric behind each one, and the reported-only columns the design cut from
//! v1 (B7-B10).
//!
//! The rules are the REPAIRED ones of `dev_docs/exodos-sweep-design.md` §9.2,
//! and the acceptance oracle is the leg-A table of §9.5 over the eleven
//! fixture profiles. Nothing here may be re-tuned to make a row pass: if a
//! fixture disagrees with the table, one of the two is wrong and the
//! disagreement is the finding.
//!
//! ## The classification window
//!
//! Marks are guest-clock driven, not wall driven — MEASURED, not assumed, on
//! the smoke archive: over 60 periodic marks the guest spacing holds
//! 1999.25-2000.84 ms (0.08%) while the wall spacing between the same marks
//! ranges 615 ms to 5,114 ms (8.3x). The window is therefore a
//! guest-deterministic input.
//!
//! The window is the delta between the last mark and the mark nearest to 60
//! guest seconds earlier. Only `B1`, `B2` and `B3` can use it: the mark subset
//! carries neither the callout family, the x87 counters nor
//! `jit_direct_callout_port_v86_served`, so `B4`, `B5a`, `B5b` and `B6` are
//! whole-run only and are flagged as such.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The master clock, used only when a profile carries no usable
/// `guest_seconds` to divide by.
const MASTER_CLOCK_HZ: f64 = 5_478_000_000.0;

/// The steady window, in guest seconds (design §3.3).
pub const WINDOW_GUEST_SECONDS: f64 = 60.0;

/// Below this many phase marks no 60-guest-second window exists and the row is
/// excluded from every bucket (design §9.4).
pub const MIN_PHASE_MARKS: usize = 31;

/// Returns to the opening frame that call a row a reboot loop. The design's
/// stdout boot-banner scrape has no signal on the `--hdd-folder` path (no
/// banner is printed), and the merged correction is screen-index recurrence.
pub const REBOOT_RECURRENCE_THRESHOLD: u32 = 2;

/// Screen samples that must land inside the window before flatness may be
/// asserted at all. Three is the smallest number that can distinguish "the
/// picture never changed" from "we sampled it once".
pub const IDLE_MIN_WINDOW_SAMPLES: usize = 3;

/// Distinct frame hashes inside the window that count as flat. One means the
/// picture is literally the same bitmap for the whole steady window.
pub const IDLE_MAX_WINDOW_DISTINCT: usize = 1;

// ---------------------------------------------------------------------------
// Bucket thresholds. Every one of these is quoted from design §9.2 and is
// reproduced by `bucket_test.rs` at its edge.
// ---------------------------------------------------------------------------

/// B1: insns per entry below this is a short-block workload.
pub const B1_IPE_MAX: f64 = 4.0;
/// B1: entries per instruction above this is an entry-cost workload.
pub const B1_ENTRIES_PER_INSN_MIN: f64 = 0.05;
/// B2: interpreted share above this is a residency problem.
pub const B2_INTERP_MIN: f64 = 0.15;
/// B3: `smc_heat_demotions / instructions`. A 445x gap sits at this bar.
pub const B3_DEMOTIONS_PER_INSN_MIN: f64 = 1e-7;
/// B4: callouts per instruction. 0.015, NOT the review's 0.03, which excludes
/// both wolf rows (they read 0.0240 and 0.0218).
pub const B4_CALLOUTS_PER_INSN_MIN: f64 = 0.015;
/// B5a: x87 eligibility side exits per instruction.
pub const B5A_X87_ELIGIBILITY_PER_INSN_MIN: f64 = 1e-3;
/// B5b: x87 pad bails per instruction.
pub const B5B_X87_PAD_BAILS_PER_INSN_MIN: f64 = 1e-5;
/// B6: V86-served port callouts per instruction.
pub const B6_PORT_V86_SERVED_PER_INSN_MIN: f64 = 0.01;

/// Real-time factor at or above which a row keeps up with the persona.
pub const HEALTHY_RT_MIN: f64 = 1.0;
/// The §4.4 severity floor. Without it `HEALTHY-WITH-FINDINGS` contributes
/// nothing to class mass and the category deletes itself.
pub const SEVERITY_FLOOR: f64 = 0.1;
/// Severity's intensity term is capped here.
pub const SEVERITY_INTENSITY_CAP: f64 = 4.0;

// ---------------------------------------------------------------------------
// Profile shape. Everything defaults, because a counter added after a board
// was recorded must read zero rather than refuse the file.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stop {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub code: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Perf {
    pub instructions: u64,
    pub jit_direct_insns: u64,
    pub jit_direct_entries: u64,
    pub jit_direct_side_exits: u64,
    pub jit_direct_x87_pad_bails: u64,
    pub smc_heat_demotions: u64,
    pub smc_heat_chunks_hot: u64,
    pub smc_scan_calls: u64,
    pub smc_scan_keys: u64,
    pub jit_direct_cache_resets: u64,
    pub decode_misses: u64,
    pub decode_probes: u64,
    pub decode_inval_smc: u64,
    pub code_invalidations: u64,
    pub jit_direct_blocks_installed: u64,
    pub jit_direct_compile_attempts: u64,
    pub jit_direct_compile_ns: u64,
    pub jit_direct_linked_transfers: u64,
    pub jit_direct_unresolved_exits: u64,
    pub device_write_bytes: u64,
    pub halted_ticks: u64,
    pub io_stall_ticks: u64,
    pub brk_step: u64,
    pub straight_line_runs: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DirectStalls {
    pub jit_direct_callout_executed: u64,
    pub side_exit_callout_step_break: u64,
    pub side_exit_callout_abnormal: u64,
    pub side_exit_x87_eligibility: u64,
    pub jit_direct_callout_port_v86_served: u64,
    pub x87_top_sticky_crossings: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Katea {
    pub host_wall_ns: u64,
}

/// One periodic phase mark. Absolute, monotonically non-decreasing snapshots.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Mark {
    pub id: u8,
    pub master_ticks: u64,
    pub instructions: u64,
    pub jit_direct_insns: u64,
    pub jit_direct_entries: u64,
    pub jit_direct_side_exits: u64,
    pub smc_heat_demotions: u64,
    pub device_write_bytes: u64,
    pub halted_ticks: u64,
    pub io_stall_ticks: u64,
    pub brk_step: u64,
    pub straight_line_runs: u64,
    pub decode_misses: u64,
    pub decode_probes: u64,
    pub decode_inval_smc: u64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub schema: String,
    pub mode: String,
    pub real_time_factor: f64,
    pub guest_seconds: f64,
    pub wall_seconds: f64,
    pub master_ticks: u64,
    pub machine_phase_timing_enabled: bool,
    pub stop: Stop,
    pub perf: Perf,
    pub direct_stalls: DirectStalls,
    pub katea: Katea,
    pub phase_marks: Vec<Mark>,
}

/// One screen sample from `screens/screens.jsonl`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Screen {
    pub i: u64,
    pub master_ticks: u64,
    pub guest_ms: u64,
    pub display: String,
    pub video_mode: Option<String>,
    pub hash: String,
    pub changed: bool,
    pub text_glyphs: Option<u64>,
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// The counter deltas over the steady window, plus how the window was chosen.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Window {
    /// False when the profile carries no usable mark series at all.
    pub available: bool,
    pub mark_count: usize,
    /// Index of the mark that opens the window.
    pub base_index: usize,
    pub guest_seconds: f64,
    pub instructions: u64,
    pub jit_direct_insns: u64,
    pub jit_direct_entries: u64,
    pub jit_direct_side_exits: u64,
    pub smc_heat_demotions: u64,
    pub device_write_bytes: u64,
    pub halted_ticks: u64,
    pub io_stall_ticks: u64,
    pub master_ticks: u64,
    pub brk_step: u64,
    pub straight_line_runs: u64,
    pub decode_misses: u64,
    pub decode_probes: u64,
    /// `w_instructions / perf.instructions` — how much of the run the window
    /// covers, which is what a whole-run bucket must be discounted against.
    pub window_fraction: f64,
}

/// Master ticks per guest second for this profile.
///
/// Derived from the profile itself rather than from a constant, because the
/// only invariant the schema promises is `guest_seconds = master_ticks / HZ`.
pub fn master_hz(profile: &Profile) -> f64 {
    if profile.guest_seconds > 0.0 && profile.master_ticks > 0 {
        profile.master_ticks as f64 / profile.guest_seconds
    } else {
        MASTER_CLOCK_HZ
    }
}

/// Pick the mark that opens the last-`WINDOW_GUEST_SECONDS` window.
///
/// The last mark closes it. The opener is the mark whose `master_ticks` is
/// nearest the target, ties going to the EARLIER mark so the window is never
/// short of the requested span by a rounding accident. The final mark is never
/// its own opener.
pub fn window_base_index(marks: &[Mark], hz: f64) -> Option<usize> {
    if marks.len() < 2 {
        return None;
    }
    let last = marks.len() - 1;
    let end = marks[last].master_ticks as f64;
    let target = end - WINDOW_GUEST_SECONDS * hz;
    let mut best = 0usize;
    let mut best_distance = f64::INFINITY;
    for (index, mark) in marks.iter().enumerate().take(last) {
        let distance = (mark.master_ticks as f64 - target).abs();
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    Some(best)
}

/// Compute the window deltas. Returns an unavailable window when the series is
/// too short to span one.
pub fn compute_window(profile: &Profile) -> Window {
    let marks = &profile.phase_marks;
    let hz = master_hz(profile);
    let mut window = Window {
        mark_count: marks.len(),
        ..Window::default()
    };
    let Some(base_index) = window_base_index(marks, hz) else {
        return window;
    };
    let base = &marks[base_index];
    let last = &marks[marks.len() - 1];
    window.available = true;
    window.base_index = base_index;
    window.instructions = last.instructions.saturating_sub(base.instructions);
    window.jit_direct_insns = last.jit_direct_insns.saturating_sub(base.jit_direct_insns);
    window.jit_direct_entries = last
        .jit_direct_entries
        .saturating_sub(base.jit_direct_entries);
    window.jit_direct_side_exits = last
        .jit_direct_side_exits
        .saturating_sub(base.jit_direct_side_exits);
    window.smc_heat_demotions = last
        .smc_heat_demotions
        .saturating_sub(base.smc_heat_demotions);
    window.device_write_bytes = last
        .device_write_bytes
        .saturating_sub(base.device_write_bytes);
    window.halted_ticks = last.halted_ticks.saturating_sub(base.halted_ticks);
    window.io_stall_ticks = last.io_stall_ticks.saturating_sub(base.io_stall_ticks);
    window.master_ticks = last.master_ticks.saturating_sub(base.master_ticks);
    window.brk_step = last.brk_step.saturating_sub(base.brk_step);
    window.straight_line_runs = last
        .straight_line_runs
        .saturating_sub(base.straight_line_runs);
    window.decode_misses = last.decode_misses.saturating_sub(base.decode_misses);
    window.decode_probes = last.decode_probes.saturating_sub(base.decode_probes);
    window.guest_seconds = window.master_ticks as f64 / hz;
    window.window_fraction = if profile.perf.instructions == 0 {
        0.0
    } else {
        window.instructions as f64 / profile.perf.instructions as f64
    };
    window
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Outcome {
    Untranslatable,
    HungHost,
    Stalled,
    NoProfile,
    Crashed,
    RebootLoop,
    ShortRun,
    Halted,
    Exited,
    IdleText,
    IdleAtMenu,
    Ran,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Untranslatable => "UNTRANSLATABLE",
            Outcome::HungHost => "HUNG-HOST",
            Outcome::Stalled => "STALLED",
            Outcome::NoProfile => "NO-PROFILE",
            Outcome::Crashed => "CRASHED",
            Outcome::RebootLoop => "REBOOT-LOOP",
            Outcome::ShortRun => "SHORT-RUN",
            Outcome::Halted => "HALTED",
            Outcome::Exited => "EXITED",
            Outcome::IdleText => "IDLE-TEXT",
            Outcome::IdleAtMenu => "IDLE-AT-MENU",
            Outcome::Ran => "RAN",
        }
    }

    /// Whether a row with this outcome may carry bucket memberships.
    pub fn bucketable(self) -> bool {
        matches!(self, Outcome::Ran | Outcome::Halted | Outcome::Exited)
    }
}

/// Count returns to the opening frame — the merged reboot detector.
pub fn screen_recurrences(screens: &[Screen]) -> u32 {
    if screens.len() < 3 {
        return 0;
    }
    let first = &screens[0].hash;
    let mut returns = 0;
    let mut left = false;
    for sample in screens {
        if &sample.hash != first {
            left = true;
        } else if left {
            returns += 1;
            left = false;
        }
    }
    returns
}

/// What the screen index says about the classification window.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ScreenWindow {
    pub samples_total: usize,
    pub samples_in_window: usize,
    pub distinct_in_window: usize,
    /// The last in-window sample's mode, `None` when the display is not the
    /// VGA raster and no mode line exists (design N-3's `IDLE-BLIND`).
    pub video_mode: Option<String>,
    pub display: String,
    pub flat: bool,
}

/// Restrict the screen index to the classification window and measure how much
/// the picture moved inside it.
pub fn screen_window(screens: &[Screen], profile: &Profile) -> ScreenWindow {
    let mut out = ScreenWindow {
        samples_total: screens.len(),
        ..ScreenWindow::default()
    };
    if screens.is_empty() {
        return out;
    }
    let hz = master_hz(profile);
    let end = screens
        .iter()
        .map(|s| s.master_ticks)
        .max()
        .unwrap_or(0)
        .max(profile.master_ticks);
    let span = (WINDOW_GUEST_SECONDS * hz) as u64;
    let low = end.saturating_sub(span);
    let inside: Vec<&Screen> = screens.iter().filter(|s| s.master_ticks >= low).collect();
    out.samples_in_window = inside.len();
    let mut hashes: Vec<&str> = inside.iter().map(|s| s.hash.as_str()).collect();
    hashes.sort_unstable();
    hashes.dedup();
    out.distinct_in_window = hashes.len();
    if let Some(last) = inside.last() {
        out.video_mode = last.video_mode.clone();
        out.display = last.display.clone();
    }
    out.flat = out.samples_in_window >= IDLE_MIN_WINDOW_SAMPLES
        && out.distinct_in_window <= IDLE_MAX_WINDOW_DISTINCT
        && out.distinct_in_window > 0;
    out
}

/// Host-side facts the archive cannot carry, lifted from the orchestrator row.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostVerdict {
    pub timed_out: bool,
    pub stalled: bool,
    pub untranslatable: bool,
}

/// Halted share of the window at or above which the guest is mostly asleep.
pub const IDLE_HALTED_SHARE_MIN: f64 = 0.5;

/// Why a flat picture was, or was not, called idle.
///
/// **The counter-only flatness test the design proposed is REFUTED and this
/// records the refutation.** §3.4 offers `device_write_bytes` as the frame
/// proxy and a delta of about zero as the idle signal. Measured on the smoke
/// archive: DOOM runs the whole 120 guest seconds in mode X with its frame
/// hash changing 11 distinct times inside the window, and its window
/// `device_write_bytes` delta is exactly **0**. The counter stops accruing
/// once the VGA aperture goes through the direct-data path, so on its own it
/// would call a healthy anchor idle. The frame-hash index is therefore the
/// flatness test and the counters are the corroborating polling signature.
#[derive(Debug, Clone, Default, Serialize)]
pub struct IdleEvidence {
    /// The picture did not change across the window (frame-hash test).
    pub frame_flat: bool,
    pub window_device_write_bytes: u64,
    pub window_halted_share: f64,
    pub port_v86_served_per_insn: f64,
    /// Which corroborating term fired, empty when none did.
    pub polling_terms: Vec<String>,
    pub polling_signature: bool,
}

/// Measure the idle evidence: the frame-hash flatness plus the polling terms.
pub fn idle_evidence(picture: &ScreenWindow, profile: &Profile, window: &Window) -> IdleEvidence {
    let halted_share = if window.master_ticks == 0 {
        0.0
    } else {
        window.halted_ticks as f64 / window.master_ticks as f64
    };
    let v86 = ratio(
        profile.direct_stalls.jit_direct_callout_port_v86_served,
        profile.perf.instructions,
    );
    let mut terms = Vec::new();
    if window.available && window.device_write_bytes == 0 {
        terms.push("window_device_write_bytes==0".to_string());
    }
    if halted_share > IDLE_HALTED_SHARE_MIN {
        terms.push("window_halted_share>0.5".to_string());
    }
    if v86 > B6_PORT_V86_SERVED_PER_INSN_MIN {
        terms.push("port_v86_served/I>0.01".to_string());
    }
    IdleEvidence {
        frame_flat: picture.flat,
        window_device_write_bytes: window.device_write_bytes,
        window_halted_share: halted_share,
        port_v86_served_per_insn: v86,
        polling_signature: !terms.is_empty(),
        polling_terms: terms,
    }
}

/// Decide the outcome, in the design's order.
///
/// **Deviation from §9.4, stated on purpose.** `SHORT-RUN` is "marks were
/// armed and there were fewer than 31 of them". A profile with an EMPTY mark
/// series is a run where `IZARRAVM_PHASE_INTERVAL_MS` was never armed — every
/// in-tree fixture profile is one — and calling that `SHORT-RUN` would exclude
/// the acceptance oracle itself from every bucket. Such a row keeps its
/// counter-derived outcome and is flagged `NO-MARKS`: whole-run only, no
/// window, no window fraction to discount against.
pub fn decide_outcome(profile: Option<&Profile>, screens: &[Screen], host: HostVerdict) -> Outcome {
    if host.untranslatable {
        return Outcome::Untranslatable;
    }
    if host.timed_out {
        return Outcome::HungHost;
    }
    if host.stalled {
        return Outcome::Stalled;
    }
    let Some(profile) = profile else {
        return Outcome::NoProfile;
    };
    if profile.stop.kind == "cpu_error" {
        return Outcome::Crashed;
    }
    if screen_recurrences(screens) >= REBOOT_RECURRENCE_THRESHOLD {
        return Outcome::RebootLoop;
    }
    let marks = profile.phase_marks.len();
    if marks > 0 && marks < MIN_PHASE_MARKS {
        return Outcome::ShortRun;
    }
    if profile.stop.kind == "halted" {
        return Outcome::Halted;
    }
    if profile.stop.kind == "test_exit" || profile.stop.kind == "dos_exit" {
        return Outcome::Exited;
    }
    let picture = screen_window(screens, profile);
    let window = compute_window(profile);
    let idle = idle_evidence(&picture, profile, &window);
    if idle.frame_flat {
        // A flat picture in text mode is the DOS prompt, not a game menu.
        // With no mode line at all the display is the Margo framebuffer and
        // the design's `IDLE-BLIND` applies: we cannot say text, so we say
        // menu and flag the blindness.
        let is_text = picture
            .video_mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("text"))
            .unwrap_or(false);
        if is_text {
            return Outcome::IdleText;
        }
        if idle.polling_signature {
            return Outcome::IdleAtMenu;
        }
        // Flat picture, no polling term. The owner's rule is a conjunction, so
        // this stays `RAN` and the row carries `FLAT-PICTURE-NOT-IDLE` for
        // stage 1 to look at rather than being silently absorbed.
    }
    Outcome::Ran
}

// ---------------------------------------------------------------------------
// Buckets
// ---------------------------------------------------------------------------

/// One bucket membership, carrying the number that put it there.
#[derive(Debug, Clone, Serialize)]
pub struct BucketHit {
    pub id: &'static str,
    pub name: &'static str,
    pub metric: &'static str,
    pub value: f64,
    pub threshold: f64,
    /// True when the metric came from the steady window, false when the mark
    /// subset does not carry it and the whole run had to stand in.
    pub windowed: bool,
    pub severity: f64,
    pub lever: &'static str,
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn severity(real_time_factor: f64, value: f64, threshold: f64) -> f64 {
    let urgency = (1.0 - real_time_factor).clamp(0.0, 1.0).max(SEVERITY_FLOOR);
    let intensity = if threshold > 0.0 {
        (value / threshold).min(SEVERITY_INTENSITY_CAP)
    } else {
        1.0
    };
    urgency * intensity
}

/// The counters each bucket rule reads, already resolved to window or
/// whole-run. Split out so the arithmetic is testable without a file.
#[derive(Debug, Clone, Default)]
pub struct BucketInputs {
    /// Window instructions where a window exists, whole-run otherwise.
    pub windowed_instructions: u64,
    pub windowed_jit_direct_insns: u64,
    pub windowed_jit_direct_entries: u64,
    pub windowed_smc_heat_demotions: u64,
    /// Whole-run instructions, the denominator for every non-windowable rule.
    pub instructions: u64,
    pub callouts: u64,
    pub side_exit_x87_eligibility: u64,
    pub x87_pad_bails: u64,
    pub callout_port_v86_served: u64,
    pub real_time_factor: f64,
    pub windowed: bool,
}

impl BucketInputs {
    pub fn from_profile(profile: &Profile, window: &Window) -> Self {
        let perf = &profile.perf;
        let stalls = &profile.direct_stalls;
        let use_window = window.available && window.instructions > 0;
        Self {
            windowed_instructions: if use_window {
                window.instructions
            } else {
                perf.instructions
            },
            windowed_jit_direct_insns: if use_window {
                window.jit_direct_insns
            } else {
                perf.jit_direct_insns
            },
            windowed_jit_direct_entries: if use_window {
                window.jit_direct_entries
            } else {
                perf.jit_direct_entries
            },
            windowed_smc_heat_demotions: if use_window {
                window.smc_heat_demotions
            } else {
                perf.smc_heat_demotions
            },
            instructions: perf.instructions,
            callouts: stalls.jit_direct_callout_executed
                + stalls.side_exit_callout_step_break
                + stalls.side_exit_callout_abnormal,
            side_exit_x87_eligibility: stalls.side_exit_x87_eligibility,
            x87_pad_bails: perf.jit_direct_x87_pad_bails,
            callout_port_v86_served: stalls.jit_direct_callout_port_v86_served,
            real_time_factor: profile.real_time_factor,
            windowed: use_window,
        }
    }

    pub fn insns_per_entry(&self) -> f64 {
        self.windowed_jit_direct_insns as f64 / self.windowed_jit_direct_entries.max(1) as f64
    }

    pub fn entries_per_insn(&self) -> f64 {
        ratio(self.windowed_jit_direct_entries, self.windowed_instructions)
    }

    pub fn interpreter_share(&self) -> f64 {
        if self.windowed_instructions == 0 {
            return 0.0;
        }
        1.0 - ratio(self.windowed_jit_direct_insns, self.windowed_instructions)
    }
}

/// Apply the six repaired rules. Order is B1, B2, B3, B4, B5a, B5b, B6.
pub fn buckets(inputs: &BucketInputs) -> Vec<BucketHit> {
    let mut hits = Vec::new();
    if inputs.instructions == 0 || inputs.windowed_instructions == 0 {
        // A run that executed nothing has no structure to bucket. Every rule
        // below is a rate per instruction and would otherwise divide a real
        // numerator by nothing, or read `1 - 0/0` as a total interpreter miss.
        return hits;
    }
    let rt = inputs.real_time_factor;

    let ipe = inputs.insns_per_entry();
    let entries_per_insn = inputs.entries_per_insn();
    if ipe < B1_IPE_MAX && entries_per_insn > B1_ENTRIES_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B1",
            name: "entry-cost / short-blocks",
            metric: "jit_direct_insns/entries (with entries/instructions)",
            value: ipe,
            threshold: B1_IPE_MAX,
            windowed: inputs.windowed,
            // B1's rule is a floor, not a ceiling: intensity is how far BELOW
            // the bar the ratio sits, so a game at ipe 1 is worse than one at
            // ipe 3.9. Inverting keeps "bigger is worse" true for severity.
            severity: severity(rt, B1_IPE_MAX / ipe.max(f64::MIN_POSITIVE), 1.0),
            lever: "block extension, warm-fetch-inline, admission governor",
        });
    }

    let interp = inputs.interpreter_share();
    if interp > B2_INTERP_MIN {
        hits.push(BucketHit {
            id: "B2",
            name: "residency / interpreter share",
            metric: "1 - jit_direct_insns/instructions",
            value: interp,
            threshold: B2_INTERP_MIN,
            windowed: inputs.windowed,
            severity: severity(rt, interp, B2_INTERP_MIN),
            lever: "admission breadth, split by the dominant reject row",
        });
    }

    let demotions = ratio(
        inputs.windowed_smc_heat_demotions,
        inputs.windowed_instructions,
    );
    if demotions > B3_DEMOTIONS_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B3",
            name: "SMC-refusal",
            metric: "smc_heat_demotions/instructions",
            value: demotions,
            threshold: B3_DEMOTIONS_PER_INSN_MIN,
            windowed: inputs.windowed,
            severity: severity(rt, demotions, B3_DEMOTIONS_PER_INSN_MIN),
            lever: "parameterized blocks, mutable imm lanes, one-byte imm lane",
        });
    }

    let callouts = ratio(inputs.callouts, inputs.instructions);
    if callouts > B4_CALLOUTS_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B4",
            name: "callout-heavy",
            metric: "(callout_executed + step_break + abnormal)/instructions",
            value: callouts,
            threshold: B4_CALLOUTS_PER_INSN_MIN,
            windowed: false,
            severity: severity(rt, callouts, B4_CALLOUTS_PER_INSN_MIN),
            lever: "callout governor, lazy gameport reads",
        });
    }

    let eligibility = ratio(inputs.side_exit_x87_eligibility, inputs.instructions);
    if eligibility > B5A_X87_ELIGIBILITY_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B5a",
            name: "x87-turnover",
            metric: "side_exit_x87_eligibility/instructions",
            value: eligibility,
            threshold: B5A_X87_ELIGIBILITY_PER_INSN_MIN,
            windowed: false,
            severity: severity(rt, eligibility, B5A_X87_ELIGIBILITY_PER_INSN_MIN),
            lever: "x87 top-of-stack tracking, top-retire policy",
        });
    }

    let pad_bails = ratio(inputs.x87_pad_bails, inputs.instructions);
    if pad_bails > B5B_X87_PAD_BAILS_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B5b",
            name: "x87 pad-bail",
            metric: "jit_direct_x87_pad_bails/instructions",
            value: pad_bails,
            threshold: B5B_X87_PAD_BAILS_PER_INSN_MIN,
            windowed: false,
            severity: severity(rt, pad_bails, B5B_X87_PAD_BAILS_PER_INSN_MIN),
            lever: "x87 pad sizing, shared re-entry pad",
        });
    }

    let v86 = ratio(inputs.callout_port_v86_served, inputs.instructions);
    if v86 > B6_PORT_V86_SERVED_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B6",
            name: "port-polling / V86-served I/O",
            metric: "jit_direct_callout_port_v86_served/instructions",
            value: v86,
            threshold: B6_PORT_V86_SERVED_PER_INSN_MIN,
            windowed: false,
            severity: severity(rt, v86, B6_PORT_V86_SERVED_PER_INSN_MIN),
            lever: "poll-skip, analytic PIT/3DA peeks, device edge cache",
        });
    }

    hits
}

// ---------------------------------------------------------------------------
// Reported-only columns (the cut buckets B7-B10)
// ---------------------------------------------------------------------------

/// The four cut buckets survive as columns so stage 1 can discover a corpus
/// value the fixtures never reached. None of these decides anything.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReportedColumns {
    /// B7. `decode_misses/instructions` and the closer analogue
    /// `decode_misses/decode_probes`, which is HIGHEST on doom-486.
    pub b7_decode_misses_per_insn: f64,
    pub b7_decode_miss_rate: f64,
    /// B8. `katea.host_wall_ns / wall_ns`.
    pub b8_katea_ratio: f64,
    /// B9. compile time against wall, and the install rate.
    pub b9_compile_ns_ratio: f64,
    pub b9_installed_per_attempt: f64,
    /// B10. links per side exit, and unresolved exits per side exit.
    pub b10_linked_per_side_exit: f64,
    pub b10_unresolved_per_side_exit: f64,
}

pub fn reported_columns(profile: &Profile) -> ReportedColumns {
    let perf = &profile.perf;
    let wall_ns = (profile.wall_seconds * 1e9) as u64;
    ReportedColumns {
        b7_decode_misses_per_insn: ratio(perf.decode_misses, perf.instructions),
        b7_decode_miss_rate: ratio(perf.decode_misses, perf.decode_probes),
        b8_katea_ratio: ratio(profile.katea.host_wall_ns, wall_ns),
        b9_compile_ns_ratio: ratio(perf.jit_direct_compile_ns, wall_ns),
        b9_installed_per_attempt: ratio(
            perf.jit_direct_blocks_installed,
            perf.jit_direct_compile_attempts,
        ),
        b10_linked_per_side_exit: ratio(
            perf.jit_direct_linked_transfers,
            perf.jit_direct_side_exits,
        ),
        b10_unresolved_per_side_exit: ratio(
            perf.jit_direct_unresolved_exits,
            perf.jit_direct_side_exits,
        ),
    }
}

// ---------------------------------------------------------------------------
// The row
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Health {
    Healthy,
    HealthyWithFindings,
    NonHealthy,
    Excluded,
}

impl Health {
    pub fn as_str(self) -> &'static str {
        match self {
            Health::Healthy => "HEALTHY",
            Health::HealthyWithFindings => "HEALTHY-WITH-FINDINGS",
            Health::NonHealthy => "NON-HEALTHY",
            Health::Excluded => "EXCLUDED",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ClassifiedRow {
    pub short: String,
    pub persona: String,
    pub outcome: &'static str,
    pub stop_kind: String,
    pub stop_code: Option<i64>,
    pub stop_message: Option<String>,
    pub health: &'static str,
    pub real_time_factor: f64,
    pub guest_seconds: f64,
    pub wall_seconds: f64,
    pub phase_mark_count: usize,
    pub screen_recurrences: u32,
    pub flags: Vec<String>,
    pub buckets: Vec<String>,
    pub bucket_hits: Vec<BucketHit>,
    pub window: Window,
    pub screens: ScreenWindow,
    pub idle: IdleEvidence,
    pub instructions: u64,
    pub direct_native_coverage: f64,
    pub insns_per_entry: f64,
    pub entries_per_insn: f64,
    pub interpreter_share: f64,
    pub reported: ReportedColumns,
}

/// Everything one archived game contributes.
#[derive(Debug, Clone, Default)]
pub struct Archive {
    pub short: String,
    pub profile: Option<Profile>,
    pub screens: Vec<Screen>,
    pub host: HostVerdict,
    /// Flags the orchestrator already decided (`B6-BLIND`, `WANTS-GUS`, ...).
    pub flags: Vec<String>,
}

/// Classify one archived game.
pub fn classify_archive(archive: &Archive) -> ClassifiedRow {
    let outcome = decide_outcome(archive.profile.as_ref(), &archive.screens, archive.host);
    let profile = archive.profile.clone().unwrap_or_default();
    let window = compute_window(&profile);
    let inputs = BucketInputs::from_profile(&profile, &window);
    let picture = screen_window(&archive.screens, &profile);
    let idle = idle_evidence(&picture, &profile, &window);

    let mut flags = archive.flags.clone();
    if idle.frame_flat && outcome == Outcome::Ran {
        flags.push("FLAT-PICTURE-NOT-IDLE".to_string());
    }
    if profile.phase_marks.is_empty() {
        flags.push("NO-MARKS".to_string());
    } else if !window.available {
        flags.push("NO-WINDOW".to_string());
    }
    if !inputs.windowed && !profile.phase_marks.is_empty() {
        flags.push("WHOLE-RUN-FALLBACK".to_string());
    }
    if picture.samples_total > 0 && picture.video_mode.is_none() {
        flags.push("IDLE-BLIND".to_string());
    }
    if profile.machine_phase_timing_enabled {
        // Arming marks must not arm phase timing (design §9.5 step 4).
        flags.push("PHASE-TIMING-ARMED".to_string());
    }
    flags.sort();
    flags.dedup();

    let hits = if outcome.bucketable() {
        buckets(&inputs)
    } else {
        Vec::new()
    };
    let names: Vec<String> = hits.iter().map(|hit| hit.id.to_string()).collect();

    let health = if !outcome.bucketable() {
        Health::Excluded
    } else if hits.is_empty() {
        if profile.real_time_factor >= HEALTHY_RT_MIN {
            Health::Healthy
        } else {
            Health::NonHealthy
        }
    } else if profile.real_time_factor >= HEALTHY_RT_MIN {
        Health::HealthyWithFindings
    } else {
        Health::NonHealthy
    };

    ClassifiedRow {
        short: archive.short.clone(),
        persona: profile.mode.clone(),
        outcome: outcome.as_str(),
        stop_kind: profile.stop.kind.clone(),
        stop_code: profile.stop.code,
        stop_message: profile.stop.message.clone(),
        health: health.as_str(),
        real_time_factor: profile.real_time_factor,
        guest_seconds: profile.guest_seconds,
        wall_seconds: profile.wall_seconds,
        phase_mark_count: profile.phase_marks.len(),
        screen_recurrences: screen_recurrences(&archive.screens),
        flags,
        buckets: names,
        bucket_hits: hits,
        window,
        screens: picture,
        idle,
        instructions: profile.perf.instructions,
        direct_native_coverage: ratio(profile.perf.jit_direct_insns, profile.perf.instructions),
        insns_per_entry: inputs.insns_per_entry(),
        entries_per_insn: inputs.entries_per_insn(),
        interpreter_share: inputs.interpreter_share(),
        reported: reported_columns(&profile),
    }
}

// ---------------------------------------------------------------------------
// Reading an archive off disk
// ---------------------------------------------------------------------------

/// Read the screen index, tolerating a truncated final line (the orchestrator
/// appends as the run goes and a killed run can leave a partial record).
pub fn read_screens(path: &Path) -> Vec<Screen> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Screen>(line).ok())
        .collect()
}

/// The orchestrator's own row, for the host-side facts no archive carries.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SweepRow {
    pub short: String,
    pub outcome: String,
    pub flags: Vec<String>,
}

fn read_sweep_rows(dir: &Path) -> BTreeMap<String, SweepRow> {
    let mut map = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(dir.join("rows.jsonl")) else {
        return map;
    };
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if let Ok(row) = serde_json::from_str::<SweepRow>(line) {
            map.insert(row.short.clone(), row);
        }
    }
    map
}

/// Load one game directory: `profile.json` plus `screens/screens.jsonl`.
pub fn load_game_dir(dir: &Path, row: Option<&SweepRow>) -> Archive {
    let short = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let profile = std::fs::read_to_string(dir.join("profile.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Profile>(&text).ok());
    let screens = read_screens(&dir.join("screens").join("screens.jsonl"));
    let host = row.map(host_verdict).unwrap_or_default();
    let flags = row.map(|row| row.flags.clone()).unwrap_or_default();
    Archive {
        short,
        profile,
        screens,
        host,
        flags,
    }
}

fn host_verdict(row: &SweepRow) -> HostVerdict {
    HostVerdict {
        timed_out: row.outcome == "HUNG-HOST",
        stalled: row.outcome == "STALLED",
        untranslatable: row.outcome == "UNTRANSLATABLE",
    }
}

/// Load a bare profile JSON as a game named by its file stem. This is how the
/// fixture board — eleven `<fixture>.json` files with no archive around them —
/// enters the classifier, and it is the acceptance path.
pub fn load_profile_file(path: &Path) -> Archive {
    let short = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let profile = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Profile>(&text).ok());
    Archive {
        short,
        profile,
        ..Archive::default()
    }
}

/// Resolve whatever the caller pointed at into a list of archives.
///
/// Three shapes, in order: one game directory (`profile.json` at the top), a
/// sweep output directory (game subdirectories, optionally with `rows.jsonl`),
/// or a directory of bare profile JSONs (the fixture board).
pub fn load_input(root: &Path) -> Result<Vec<Archive>, String> {
    if root.is_file() {
        return Ok(vec![load_profile_file(root)]);
    }
    if !root.is_dir() {
        return Err(format!("not a file or directory: {}", root.display()));
    }
    if root.join("profile.json").is_file() {
        return Ok(vec![load_game_dir(root, None)]);
    }

    let rows = read_sweep_rows(root);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|err| format!("{}: {err}", root.display()))?;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() && path.join("profile.json").is_file() {
            dirs.push(path);
        } else if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    dirs.sort();
    files.sort();

    if !dirs.is_empty() {
        return Ok(dirs
            .iter()
            .map(|dir| {
                let short = dir.file_name().map(|n| n.to_string_lossy().to_string());
                let row = short.as_deref().and_then(|short| rows.get(short));
                load_game_dir(dir, row)
            })
            .collect());
    }

    // The fixture-board shape. Only files that actually parse as a profile
    // with a schema line count, so a `sweep.json` sitting beside them is
    // skipped rather than classified as a game.
    let archives: Vec<Archive> = files
        .iter()
        .map(|path| load_profile_file(path))
        .filter(|archive| {
            archive
                .profile
                .as_ref()
                .is_some_and(|profile| profile.schema.starts_with("izarravm-hdd-profile"))
        })
        .collect();
    if archives.is_empty() {
        return Err(format!("no profiles found under {}", root.display()));
    }
    Ok(archives)
}

/// The eyeball table. One line per game, buckets pipe-separated.
pub fn rows_to_tsv(rows: &[ClassifiedRow]) -> String {
    let mut out = String::from(
        "short\toutcome\thealth\trt\tmarks\twindow_fraction\tbuckets\tipe\tentries_per_insn\t\
         interp\tb7_decode_miss_rate\tb8_katea_ratio\tb9_installed_per_attempt\t\
         b10_linked_per_side_exit\tflags\n",
    );
    for row in rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{:.4}\t{}\t{:.4}\t{}\t{:.3}\t{:.5}\t{:.4}\t{:.5}\t{:.6}\t{:.4}\t{:.2}\t{}\n",
            row.short,
            row.outcome,
            row.health,
            row.real_time_factor,
            row.phase_mark_count,
            row.window.window_fraction,
            row.buckets.join("|"),
            row.insns_per_entry,
            row.entries_per_insn,
            row.interpreter_share,
            row.reported.b7_decode_miss_rate,
            row.reported.b8_katea_ratio,
            row.reported.b9_installed_per_attempt,
            row.reported.b10_linked_per_side_exit,
            row.flags.join("|"),
        ));
    }
    out
}

#[cfg(test)]
#[path = "bucket_test.rs"]
mod tests;
