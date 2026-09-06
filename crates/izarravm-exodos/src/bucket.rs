// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Structural-bucket classification of one archived sweep row.
//!
//! The orchestrator (`scripts/sweep-exodos.ps1`) collects; this decides. It
//! turns an IzarraVM HDD profile JSON, its phase-mark series, its screen
//! index and the frames that index kept into an outcome, a set of bucket flags
//! with the measured metric behind each one, and reported-only columns.
//!
//! The rules are the REPAIRED ones of `dev_docs/exodos-sweep-design.md` §9.2,
//! and the acceptance oracle is the leg-A table of §9.5 over the eleven
//! fixture profiles. Nothing here may be re-tuned to make a row pass: if a
//! fixture disagrees with the table, one of the two is wrong and the
//! disagreement is the finding.
//!
//! ## v2, 2026-08-17: what the 200-game stage-1 sweep changed
//!
//! Six changes, each carrying its own measurement in the item it touches:
//!
//! 1. `REBOOT-LOOP` no longer reads the opening frame's hash. v1's rule was 0/8
//!    true positives; it now requires arrivals at the Toka-DOS boot banner. See
//!    `boot_banner_entries`.
//! 2. Flatness reads PIXELS, not frame hashes. One blinking text cursor is two
//!    hashes and one picture. See `PIXEL_DELTA_FLOOR`.
//! 3. `B4` and `B6` are one bucket. See `B6_PORT_V86_SERVED_PER_INSN_MIN`.
//! 4. `B7` is restored on corpus evidence. See `B7_DECODE_MISSES_PER_INSN_MIN`.
//! 5. `B11` is new and names the V86-monitor tax, which is the whole cost of the
//!    six rows v1 left with no bucket at all. See `B11_MONITOR_SHARE_MIN`.
//! 6. `IDLE-BLIND` now means the picture is one colour; the display-path fact it
//!    used to mean is `NO-MODE-LINE`. See `classify_archive`.
//!
//! One triage item is REFUTED rather than fixed: B5b's counter IS emitted, and
//! reads zero because the corpus does not x87-pad-bail. See
//! `the_b5b_counter_is_emitted_and_the_corpus_genuinely_reads_zero`.
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
//! guest seconds earlier. Only `B1`, `B2`, `B3` and `B7` can use it: the mark
//! subset carries neither the callout family, the x87 counters,
//! `jit_direct_callout_port_v86_served` nor the monitor counters, so `B4`,
//! `B5a`, `B5b` and `B11` are whole-run only and are flagged as such.

use std::collections::{BTreeMap, BTreeSet};
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

/// Arrivals at the Toka-DOS boot screen that call a row a reboot loop. Two means
/// the machine booted twice.
///
/// **v1's rule was 0/8 true positives and is gone.** It counted returns to
/// `screens[0]`'s frame hash, which a blinking text cursor, an attract cycle and
/// a black fade frame all produce; it ran first in `decide_outcome`, so it also
/// excluded four otherwise-fine rows. `screen_recurrences` survives as a
/// reported column. See `boot_banner_entries` for what decides now.
pub const REBOOT_BANNER_ENTRY_THRESHOLD: u32 = 2;

/// Screen samples that must land inside the window before flatness may be
/// asserted at all. Three is the smallest number that can distinguish "the
/// picture never changed" from "we sampled it once".
pub const IDLE_MIN_WINDOW_SAMPLES: usize = 3;

/// Distinct PICTURES inside the window that count as flat. One means the picture
/// did not change for the whole steady window — where "did not change" is the
/// pixel test of `PIXEL_DELTA_FLOOR`, not frame-hash equality.
pub const IDLE_MAX_WINDOW_DISTINCT: usize = 1;

// ---------------------------------------------------------------------------
// Frame evidence. The archive keeps a PPM for every sample whose hash moved, and
// v2 reads them, because the frame hash alone cannot tell a blinking cursor from
// a new screen and stage 1 paid for that twice (§2 items 1 and 2).
// ---------------------------------------------------------------------------

/// The boot screen's geometry. Toka-DOS boots into text mode 03h, which the Vega
/// BIOS presents at 720x400.
pub const BOOT_BANNER_WIDTH: usize = 720;
/// The boot screen's height. See `BOOT_BANNER_WIDTH`.
pub const BOOT_BANNER_HEIGHT: usize = 400;

/// Pixel rows of the boot screen that carry the invariant banner: the ten text
/// rows of the ASCII logo, at 16 pixel rows per text row.
///
/// MEASURED 2026-08-17 by booting `--hdd-folder` with a 200 ms screen dump. The
/// boot screen paints the logo across text rows 0-9, then a box holding the
/// kernel build and compile date, then the per-game CONFIG.SYS and AUTOEXEC
/// echo. Only the logo is invariant across games AND across image rebuilds, so
/// the crop stops before the date box.
pub const BOOT_BANNER_ROWS: usize = 160;

/// FNV-1a of the banner crop's RGB bytes.
///
/// Historical reference from main `7ca814ee`, measured on 2026-08-17 with
/// 200 ms screen dumps. All 44 boot samples shared this crop; 29 frames in
/// 14 archived games matched it. A changed logo requires a fresh measurement.
pub const BOOT_BANNER_DIGEST: u64 = 0x9b44_c208_87b4_8025;

/// Pixels a decoded frame must carry before it counts as evidence.
///
/// Stage 1's defect E8: the dumper emitted a degenerate 1x1 PPM once. A
/// one-pixel frame is vacuously one colour, so without this it would report a
/// blank screen on a title whose screen was fine. The smallest mode the Vega
/// BIOS presents is 320x200, so 64x64 is far below anything real.
pub const MIN_FRAME_PIXELS: usize = 64 * 64;

/// Differing-pixel fraction at or below which two frames are the same picture.
///
/// MEASURED over all 203 archived stage-1 rows, pairwise between every distinct
/// in-window frame. A blinking text cursor differs by exactly 18 pixels of
/// 288,000 (the 9x2 underline cell). The widest pair this absorbs is 464 px
/// (0.161%, 1.55x below the bar) and the narrowest it rejects is 1,035 px
/// (0.359%, 1.44x above). 0.25% of a 720x400 text frame is five 9x16 character
/// cells, which is the unit the picture is drawn in.
pub const PIXEL_DELTA_FLOOR: f64 = 0.0025;

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
/// B4's second clause, formerly bucket B6: V86-served port callouts per
/// instruction.
///
/// **B4 and B6 are one bucket in v2.** Stage 1 measured `port_v86_served` at 98%
/// or more of all callouts on 35 of the 51 rows that had any callouts, so two
/// rules were reading one mechanism and each row that had it was counted into
/// class mass twice. The bar keeps its own name and value because the two
/// numerators are different counters with different scales; only the VERDICT is
/// merged.
pub const B6_PORT_V86_SERVED_PER_INSN_MIN: f64 = 0.01;
/// B7: `decode_misses/instructions`.
///
/// **Restored in v2 at the design's original bar.** v1 cut it for firing 0/11 on
/// the fixture board (the board tops out at 0.0148 on duke3d-486). The corpus
/// disagrees: Drilling reads 0.678, which is 13.6x the bar, and it is a RAN row
/// at rt 0.11. A bucket the anchors cannot reach is not the same thing as a
/// bucket nothing reaches.
pub const B7_DECODE_MISSES_PER_INSN_MIN: f64 = 0.05;
/// B11: share of executed CPU core clocks that retired inside the ring-0
/// monitor.
///
/// This clause gives the class its meaning and does NOT separate healthy from
/// non-healthy: `monitor_resident_core_clocks` charges any instruction that
/// retires while ring-0-protected, so a DOS/4GW game running flat in ring 0
/// reads ~0.97 with no monitor involved (doom-486 0.9695, doom-586 0.9760). The
/// vec13 clause below does the separating.
pub const B11_MONITOR_SHARE_MIN: f64 = 0.5;
/// B11: V86-to-monitor entries through vector 13, per instruction.
///
/// The discriminating clause: under TOKAEMM a V86 guest reaches ring 0 only
/// through vector 13, so a high trip rate WITH high residency is the monitor tax
/// and not an extender. The bar sits between the highest excluded fixture
/// (doom-486 at 5.32e-7, 3.76x below) and the lowest corpus row the bucket
/// exists for (conqstND at 7.31e-6, 3.65x above).
pub const B11_VEC13_TRIPS_PER_INSN_MIN: f64 = 2e-6;

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
    /// Guest core clocks charged to instructions that retired while
    /// ring-0-protected. B11's residency clause.
    pub monitor_resident_core_clocks: u64,
    /// V86-to-ring-0 entries through vector 13. B11's discriminating clause.
    pub monitor_trips_vec13: u64,
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
    /// B11's denominator. Core clocks, not master ticks: the residency counter
    /// is charged in core clocks and the two scales differ per persona.
    pub executed_cpu_core_clocks: u64,
    pub machine_phase_timing_enabled: bool,
    pub stop: Stop,
    pub perf: Perf,
    pub direct_stalls: DirectStalls,
    pub katea: Katea,
    pub phase_marks: Vec<Mark>,
}

/// One screen sample from `screens/screens.jsonl`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Screen {
    pub i: u64,
    pub master_ticks: u64,
    pub guest_ms: u64,
    pub display: String,
    pub video_mode: Option<String>,
    /// Whether the guest had completed a frame when the sample was taken.
    ///
    /// False means the dumper found no frame at all: the run had not drawn its
    /// first raster, or a mode set had just dropped the last one. Such a sample
    /// is the ABSENCE of an observation, not an observation of a blank screen,
    /// and every rule here skips it. Defaults to true so an archive written
    /// before the field existed still reads correctly — every line it wrote had
    /// a frame behind it, because the dumper substituted a one-pixel image
    /// rather than admitting there was none.
    pub presented: bool,
    pub hash: Option<String>,
    pub changed: bool,
    /// The PPM file beside the index, present only when the hash moved. A sample
    /// with no file shows the same picture as the last one that had one.
    pub ppm: Option<String>,
    pub text_glyphs: Option<u64>,
}

impl Default for Screen {
    fn default() -> Self {
        Screen {
            i: 0,
            master_ticks: 0,
            guest_ms: 0,
            display: String::new(),
            video_mode: None,
            presented: true,
            hash: None,
            changed: false,
            ppm: None,
            text_glyphs: None,
        }
    }
}

impl Screen {
    /// The frame this sample observed, or `None` when it observed none.
    pub fn frame_hash(&self) -> Option<&str> {
        if !self.presented {
            return None;
        }
        self.hash.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// One decoded screen sample.
#[derive(Debug, Clone, Default)]
pub struct FrameImage {
    pub width: usize,
    pub height: usize,
    /// Three bytes per pixel, row-major, exactly as the dumper wrote them.
    pub rgb: Vec<u8>,
}

/// Parse the binary PPM the screen dumper writes (`P6`, one whitespace after the
/// maximum value, then raw RGB). A short or foreign file is refused rather than
/// half-read: a truncated frame would compare as "different" against everything.
pub fn read_ppm(bytes: &[u8]) -> Option<FrameImage> {
    if !bytes.starts_with(b"P6") {
        return None;
    }
    let mut fields = [0usize; 3];
    let mut cursor = 2usize;
    for field in fields.iter_mut() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if start == cursor {
            return None;
        }
        *field = std::str::from_utf8(&bytes[start..cursor])
            .ok()?
            .parse()
            .ok()?;
    }
    // Exactly one whitespace byte separates the header from the pixels.
    if cursor >= bytes.len() || !bytes[cursor].is_ascii_whitespace() {
        return None;
    }
    cursor += 1;
    let (width, height) = (fields[0], fields[1]);
    let need = width.checked_mul(height)?.checked_mul(3)?;
    if width == 0 || height == 0 || bytes.len() < cursor + need {
        return None;
    }
    Some(FrameImage {
        width,
        height,
        rgb: bytes[cursor..cursor + need].to_vec(),
    })
}

impl FrameImage {
    /// FNV-1a of the boot-banner crop, or `None` when this is not a frame of the
    /// boot screen's geometry. A graphics frame can therefore never be mistaken
    /// for a boot screen whatever its bytes hash to.
    pub fn banner_digest(&self) -> Option<u64> {
        if self.width != BOOT_BANNER_WIDTH || self.height != BOOT_BANNER_HEIGHT {
            return None;
        }
        let crop = self.rgb.get(..BOOT_BANNER_WIDTH * BOOT_BANNER_ROWS * 3)?;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in crop {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(hash)
    }

    /// Whether this frame carries the Toka-DOS boot banner.
    pub fn is_boot_banner(&self) -> bool {
        self.banner_digest() == Some(BOOT_BANNER_DIGEST)
    }

    /// Pixels that differ, or `None` when the geometries do not match. A mode
    /// change is not a pixel delta and must not be reported as one.
    pub fn differing_pixels(&self, other: &FrameImage) -> Option<usize> {
        if self.width != other.width || self.height != other.height {
            return None;
        }
        Some(
            self.rgb
                .as_chunks::<3>()
                .0
                .iter()
                .zip(other.rgb.as_chunks::<3>().0)
                .filter(|(a, b)| a != b)
                .count(),
        )
    }

    /// Whether this frame is big enough to be evidence. See `MIN_FRAME_PIXELS`.
    pub fn usable(&self) -> bool {
        self.width * self.height >= MIN_FRAME_PIXELS
    }

    /// Every pixel one colour. The stage-1 ledger's E6 is a title showing a
    /// blank screen while 29.2 billion instructions spin on port reads, and no
    /// counter says so.
    pub fn blank(&self) -> bool {
        let mut pixels = self.rgb.as_chunks::<3>().0.iter();
        let Some(first) = pixels.next() else {
            return false;
        };
        pixels.all(|pixel| pixel == first)
    }
}

/// What the pixels say about each distinct frame hash in one run.
///
/// Split out from the images so the rules are testable without files: a test
/// states the differing-pixel fraction it wants and the rule reads it.
#[derive(Debug, Clone, Default)]
pub struct FrameFacts {
    /// Frame hash to "carries the boot banner".
    pub banner: BTreeMap<String, bool>,
    /// Frame hash to "every pixel one colour".
    pub blank: BTreeMap<String, bool>,
    /// Differing-pixel fraction between two hashes. The key is ordered, so a
    /// lookup must order its arguments; `same_picture` does.
    pub delta: BTreeMap<(String, String), f64>,
    /// Hashes whose picture could not be read. Never collapsed with anything.
    pub unreadable: BTreeSet<String>,
}

impl FrameFacts {
    /// Whether two frame hashes show the same picture.
    ///
    /// A hash is always the same picture as itself. Otherwise the pixels must
    /// have been measured AND the delta must sit at or below the floor: an
    /// unmeasured or unreadable pair is never asserted to be the same, so a
    /// missing PPM costs the row its collapsing rather than its honesty.
    pub fn same_picture(&self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        if self.unreadable.contains(a) || self.unreadable.contains(b) {
            return false;
        }
        let key = if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        };
        self.delta
            .get(&key)
            .is_some_and(|delta| *delta <= PIXEL_DELTA_FLOOR)
    }

    /// Measure the facts for one run from its decoded frames and the pairs worth
    /// comparing. Deltas are computed only for `compare`, because a whole-run
    /// pairwise sweep costs a frame compare per pair and only the window's frames
    /// decide flatness.
    pub fn measure(images: &BTreeMap<String, FrameImage>, compare: &[String]) -> FrameFacts {
        let mut facts = FrameFacts::default();
        for (hash, image) in images {
            facts.banner.insert(hash.clone(), image.is_boot_banner());
            facts.blank.insert(hash.clone(), image.blank());
        }
        for (index, a) in compare.iter().enumerate() {
            for b in compare.iter().skip(index + 1) {
                let (Some(left), Some(right)) = (images.get(a), images.get(b)) else {
                    continue;
                };
                let Some(differing) = left.differing_pixels(right) else {
                    continue;
                };
                let pixels = (left.width * left.height).max(1);
                let key = if a <= b {
                    (a.clone(), b.clone())
                } else {
                    (b.clone(), a.clone())
                };
                facts.delta.insert(key, differing as f64 / pixels as f64);
            }
        }
        facts
    }
}

/// Group the samples into pictures, returning one class index per sample.
///
/// Grouping is by CLIQUE and never by chain: a hash joins an existing class only
/// when it is the same picture as every member. Transitive closure would let a
/// slow pan across thirteen samples collapse into "the picture never changed",
/// one small step at a time.
/// One class index per OBSERVED sample, in sample order. A sample that observed
/// no frame contributes nothing: it is a gap in the record, not a picture.
pub fn frame_classes(screens: &[Screen], facts: &FrameFacts) -> Vec<usize> {
    let mut order: Vec<&str> = Vec::new();
    for hash in screens.iter().filter_map(Screen::frame_hash) {
        if !order.contains(&hash) {
            order.push(hash);
        }
    }
    let mut classes: Vec<Vec<&str>> = Vec::new();
    let mut class_of: BTreeMap<&str, usize> = BTreeMap::new();
    for hash in order {
        let joined = classes.iter().position(|members| {
            members
                .iter()
                .all(|member| facts.same_picture(member, hash))
        });
        let index = match joined {
            Some(index) => {
                classes[index].push(hash);
                index
            }
            None => {
                classes.push(vec![hash]);
                classes.len() - 1
            }
        };
        class_of.insert(hash, index);
    }
    screens
        .iter()
        .filter_map(Screen::frame_hash)
        .map(|hash| class_of[hash])
        .collect()
}

/// How many different values a class list holds.
pub fn distinct_count(classes: &[usize]) -> usize {
    let mut seen: Vec<usize> = classes.to_vec();
    seen.sort_unstable();
    seen.dedup();
    seen.len()
}

/// Count arrivals at the Toka-DOS boot screen — the v2 reboot detector.
///
/// An ARRIVAL is a sample carrying the banner whose predecessor does not; the
/// first sample counts when it carries the banner, because the run's own boot is
/// the first arrival. Two arrivals mean the machine booted twice.
///
/// This is the distinction v1 could not draw. The banner does not scroll off the
/// boot screen, so a run parked at the DOS prompt carries it in EVERY sample and
/// scores ONE arrival, not many; `billted` and `rogclon`, two of the eight v1
/// false positives, are exactly that shape.
/// A sample that observed no frame is SKIPPED rather than read as "no banner":
/// a gap in the record is not a departure from the boot screen, and treating it
/// as one would split a single boot into two arrivals and manufacture the very
/// verdict this rule exists to stop inventing.
pub fn boot_banner_entries(screens: &[Screen], facts: &FrameFacts) -> u32 {
    let mut entries = 0;
    let mut previous = false;
    for hash in screens.iter().filter_map(Screen::frame_hash) {
        let present = facts.banner.get(hash).copied().unwrap_or(false);
        if present && !previous {
            entries += 1;
        }
        previous = present;
    }
    entries
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
    let observed: Vec<&str> = screens.iter().filter_map(Screen::frame_hash).collect();
    if observed.len() < 3 {
        return 0;
    }
    let first = observed[0];
    let mut returns = 0;
    let mut left = false;
    for hash in observed {
        if hash != first {
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
    /// Distinct frame HASHES in the window. Kept so a v1 row and a v2 row can be
    /// compared, and because the gap between this and the picture count is the
    /// size of the blinking-cursor effect.
    pub distinct_in_window: usize,
    /// Distinct PICTURES in the window, after near-identical frames collapse.
    /// This is what flatness reads.
    pub distinct_pictures_in_window: usize,
    /// The last in-window sample's mode, `None` when the display is not the
    /// VGA raster and no mode line exists (flagged `NO-MODE-LINE`).
    pub video_mode: Option<String>,
    pub display: String,
    /// Whether the last in-window picture is a single colour.
    pub blank: bool,
    pub flat: bool,
}

/// Restrict the screen index to the classification window and measure how much
/// the picture moved inside it.
pub fn screen_window(screens: &[Screen], profile: &Profile, facts: &FrameFacts) -> ScreenWindow {
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
    // Classes are computed over the whole index so a class index means the same
    // thing throughout the run, then counted inside the window. Both series skip
    // samples that observed no frame, so their positions line up.
    let classes = frame_classes(screens, facts);
    let inside: Vec<(usize, &Screen)> = screens
        .iter()
        .filter(|s| s.frame_hash().is_some())
        .enumerate()
        .filter(|(_, s)| s.master_ticks >= low)
        .collect();
    out.samples_in_window = inside.len();
    let mut hashes: Vec<&str> = inside.iter().filter_map(|(_, s)| s.frame_hash()).collect();
    hashes.sort_unstable();
    hashes.dedup();
    out.distinct_in_window = hashes.len();
    let window_classes: Vec<usize> = inside.iter().map(|(index, _)| classes[*index]).collect();
    out.distinct_pictures_in_window = distinct_count(&window_classes);
    if let Some((_, last)) = inside.last() {
        out.video_mode = last.video_mode.clone();
        out.display = last.display.clone();
        out.blank = last
            .frame_hash()
            .and_then(|hash| facts.blank.get(hash))
            .copied()
            .unwrap_or(false);
    }
    out.flat = out.samples_in_window >= IDLE_MIN_WINDOW_SAMPLES
        && out.distinct_pictures_in_window <= IDLE_MAX_WINDOW_DISTINCT
        && out.distinct_pictures_in_window > 0;
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
pub fn decide_outcome(
    profile: Option<&Profile>,
    screens: &[Screen],
    facts: &FrameFacts,
    host: HostVerdict,
) -> Outcome {
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
    if boot_banner_entries(screens, facts) >= REBOOT_BANNER_ENTRY_THRESHOLD {
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
    let picture = screen_window(screens, profile, facts);
    let window = compute_window(profile);
    let idle = idle_evidence(&picture, profile, &window);
    if idle.frame_flat {
        // A flat picture in text mode is the DOS prompt, not a game menu.
        // With no mode line at all the display is the Margo framebuffer: we
        // cannot say text, so we say menu and flag `NO-MODE-LINE`.
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
    pub windowed_decode_misses: u64,
    /// Whole-run instructions, the denominator for every non-windowable rule.
    pub instructions: u64,
    pub callouts: u64,
    pub side_exit_x87_eligibility: u64,
    pub x87_pad_bails: u64,
    pub callout_port_v86_served: u64,
    /// B11. Whole-run only: the mark subset carries neither counter.
    pub monitor_resident_core_clocks: u64,
    pub monitor_trips_vec13: u64,
    pub core_clocks: u64,
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
            windowed_decode_misses: if use_window {
                window.decode_misses
            } else {
                perf.decode_misses
            },
            instructions: perf.instructions,
            callouts: stalls.jit_direct_callout_executed
                + stalls.side_exit_callout_step_break
                + stalls.side_exit_callout_abnormal,
            side_exit_x87_eligibility: stalls.side_exit_x87_eligibility,
            x87_pad_bails: perf.jit_direct_x87_pad_bails,
            callout_port_v86_served: stalls.jit_direct_callout_port_v86_served,
            monitor_resident_core_clocks: perf.monitor_resident_core_clocks,
            monitor_trips_vec13: perf.monitor_trips_vec13,
            core_clocks: profile.executed_cpu_core_clocks,
            real_time_factor: profile.real_time_factor,
            windowed: use_window,
        }
    }

    /// Share of executed core clocks that retired inside the ring-0 monitor.
    /// Zero when the profile carries no core-clock total, so a missing
    /// denominator cannot manufacture a bucket.
    pub fn monitor_share(&self) -> f64 {
        ratio(self.monitor_resident_core_clocks, self.core_clocks)
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

    // B4 and B6, merged. The two clauses keep their own bars because their
    // numerators are different counters; the row reports whichever clause sits
    // furthest over its bar, so a lever knows which end to pull.
    let callouts = ratio(inputs.callouts, inputs.instructions);
    let v86 = ratio(inputs.callout_port_v86_served, inputs.instructions);
    let callout_over = callouts > B4_CALLOUTS_PER_INSN_MIN;
    let v86_over = v86 > B6_PORT_V86_SERVED_PER_INSN_MIN;
    if callout_over || v86_over {
        let v86_dominates =
            v86 / B6_PORT_V86_SERVED_PER_INSN_MIN >= callouts / B4_CALLOUTS_PER_INSN_MIN;
        let (metric, value, threshold) = if v86_dominates {
            (
                "jit_direct_callout_port_v86_served/instructions",
                v86,
                B6_PORT_V86_SERVED_PER_INSN_MIN,
            )
        } else {
            (
                "(callout_executed + step_break + abnormal)/instructions",
                callouts,
                B4_CALLOUTS_PER_INSN_MIN,
            )
        };
        hits.push(BucketHit {
            id: "B4",
            name: "callout / port-polling",
            metric,
            value,
            threshold,
            windowed: false,
            severity: severity(rt, value, threshold),
            lever: "callout governor, lazy gameport reads, poll-skip, \
                    analytic PIT/3DA peeks, device edge cache",
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

    let decode_misses = ratio(inputs.windowed_decode_misses, inputs.windowed_instructions);
    if decode_misses > B7_DECODE_MISSES_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B7",
            name: "decode-cache footprint",
            metric: "decode_misses/instructions",
            value: decode_misses,
            threshold: B7_DECODE_MISSES_PER_INSN_MIN,
            windowed: inputs.windowed,
            severity: severity(rt, decode_misses, B7_DECODE_MISSES_PER_INSN_MIN),
            lever: "decode-table tags, decode-cache sizing, code-watch granularity",
        });
    }

    let monitor_share = inputs.monitor_share();
    let vec13 = ratio(inputs.monitor_trips_vec13, inputs.instructions);
    if monitor_share > B11_MONITOR_SHARE_MIN && vec13 > B11_VEC13_TRIPS_PER_INSN_MIN {
        hits.push(BucketHit {
            id: "B11",
            name: "V86-monitor residency",
            metric: "monitor_trips_vec13/instructions (with monitor_resident/core_clocks)",
            value: vec13,
            threshold: B11_VEC13_TRIPS_PER_INSN_MIN,
            windowed: false,
            severity: severity(rt, vec13, B11_VEC13_TRIPS_PER_INSN_MIN),
            lever: "V86 sensitive-op gates, monitor fast paths, TOKAEMM trap reduction",
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
    /// v1's reboot detector, kept as a column so a v1 row and a v2 row can be
    /// compared. It decides nothing.
    pub screen_recurrences: u32,
    /// v2's reboot detector: arrivals at the Toka-DOS boot screen.
    pub boot_banner_entries: u32,
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
    /// What the kept PPMs say about each distinct frame hash.
    pub frames: FrameFacts,
}

/// Classify one archived game.
pub fn classify_archive(archive: &Archive) -> ClassifiedRow {
    let outcome = decide_outcome(
        archive.profile.as_ref(),
        &archive.screens,
        &archive.frames,
        archive.host,
    );
    let profile = archive.profile.clone().unwrap_or_default();
    let window = compute_window(&profile);
    let inputs = BucketInputs::from_profile(&profile, &window);
    let picture = screen_window(&archive.screens, &profile, &archive.frames);
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
    // `NO-MODE-LINE` is a fact about the DISPLAY PATH: the Margo framebuffer
    // publishes no VGA mode, so the text-versus-graphics split cannot be made.
    // `IDLE-BLIND` is a fact about the PICTURE: it is one colour. v1 raised
    // `IDLE-BLIND` for the first of these, stage 1 read it as the second, and
    // defect E6 needed the second for real.
    if picture.samples_total > 0 && picture.video_mode.is_none() {
        flags.push("NO-MODE-LINE".to_string());
    }
    if picture.blank {
        flags.push("IDLE-BLIND".to_string());
    }
    if !archive.frames.unreadable.is_empty() {
        flags.push("FRAMES-UNREADABLE".to_string());
    }
    if picture.distinct_pictures_in_window < picture.distinct_in_window {
        // The row's frame hashes over-count its pictures. Worth a column: this
        // is the blinking-cursor effect, and its size is the gap.
        flags.push("FRAMES-COLLAPSED".to_string());
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
        boot_banner_entries: boot_banner_entries(&archive.screens, &archive.frames),
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

/// Resolve which PPM shows each distinct frame hash, and which hashes matter for
/// a pairwise pixel comparison.
///
/// A sample with no PPM shows the same picture as the last sample that had one,
/// so the file is CARRIED FORWARD. Only the classification window's hashes go
/// into the compare list: a pairwise sweep costs one frame compare per pair, and
/// only the window decides flatness.
fn frame_sources(screens: &[Screen], profile: &Profile) -> (BTreeMap<String, String>, Vec<String>) {
    let hz = master_hz(profile);
    let end = screens
        .iter()
        .map(|s| s.master_ticks)
        .max()
        .unwrap_or(0)
        .max(profile.master_ticks);
    let low = end.saturating_sub((WINDOW_GUEST_SECONDS * hz) as u64);
    let mut sources: BTreeMap<String, String> = BTreeMap::new();
    let mut compare: Vec<String> = Vec::new();
    let mut carried: Option<&str> = None;
    for sample in screens {
        if let Some(name) = &sample.ppm {
            carried = Some(name.as_str());
        }
        let Some(hash) = sample.frame_hash() else {
            // No frame, so no picture to carry forward and nothing to compare.
            continue;
        };
        if let Some(name) = carried {
            sources
                .entry(hash.to_string())
                .or_insert_with(|| name.to_string());
        }
        if sample.master_ticks >= low && !compare.iter().any(|seen| seen == hash) {
            compare.push(hash.to_string());
        }
    }
    (sources, compare)
}

/// Load one game directory: `profile.json`, `screens/screens.jsonl` and the
/// kept frames.
pub fn load_game_dir(dir: &Path, row: Option<&SweepRow>) -> Archive {
    let short = dir
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let profile = std::fs::read_to_string(dir.join("profile.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Profile>(&text).ok());
    let screens_dir = dir.join("screens");
    let screens = read_screens(&screens_dir.join("screens.jsonl"));
    let frames = read_frames(&screens_dir, &screens, profile.as_ref());
    let host = row.map(host_verdict).unwrap_or_default();
    let flags = row.map(|row| row.flags.clone()).unwrap_or_default();
    Archive {
        short,
        profile,
        screens,
        host,
        flags,
        frames,
    }
}

/// Decode the kept frames and measure the facts the rules read.
///
/// A hash whose PPM is missing, unreadable or degenerate goes into `unreadable`
/// rather than being silently skipped: without pixels the row keeps v1's
/// hash-count behaviour, and the flag says why.
pub fn read_frames(
    screens_dir: &Path,
    screens: &[Screen],
    profile: Option<&Profile>,
) -> FrameFacts {
    if screens.is_empty() {
        return FrameFacts::default();
    }
    let blank_profile = Profile::default();
    let (sources, compare) = frame_sources(screens, profile.unwrap_or(&blank_profile));
    let mut images: BTreeMap<String, FrameImage> = BTreeMap::new();
    let mut unreadable: BTreeSet<String> = BTreeSet::new();
    for hash in screens.iter().filter_map(Screen::frame_hash) {
        if images.contains_key(hash) || unreadable.contains(hash) {
            continue;
        }
        let decoded = sources
            .get(hash)
            .and_then(|name| std::fs::read(screens_dir.join(name)).ok())
            .and_then(|bytes| read_ppm(&bytes));
        match decoded.filter(FrameImage::usable) {
            Some(image) => {
                images.insert(hash.to_string(), image);
            }
            None => {
                unreadable.insert(hash.to_string());
            }
        }
    }
    let mut facts = FrameFacts::measure(&images, &compare);
    facts.unreadable = unreadable;
    facts
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
         interp\tdistinct_hashes\tdistinct_pictures\tbanner_entries\trecurrences\t\
         b7_decode_misses_per_insn\tb8_katea_ratio\tb9_installed_per_attempt\t\
         b10_linked_per_side_exit\tflags\n",
    );
    for row in rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{:.4}\t{}\t{:.4}\t{}\t{:.3}\t{:.5}\t{:.4}\t{}\t{}\t{}\t{}\t\
             {:.5}\t{:.6}\t{:.4}\t{:.2}\t{}\n",
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
            row.screens.distinct_in_window,
            row.screens.distinct_pictures_in_window,
            row.boot_banner_entries,
            row.screen_recurrences,
            row.reported.b7_decode_misses_per_insn,
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
