// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The armed half of the entry-attribution observer: the thread-local tally, the three stamp
//! operations, the histograms and the snapshot the exporter reads.
//!
//! Compiled only under `direct-entry-attribution`. See the parent module for why the state is
//! thread-local rather than a `CpuGsw` field.

use std::cell::UnsafeCell;
use std::time::Instant;

/// The sixteen buckets of design §3, in source order. `Outliers` is never marked directly: it
/// holds the excess a clamped mark shed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Phase {
    DispatchGates = 0,
    Key = 1,
    Probe = 2,
    EntryGuards = 3,
    SegmentLayout = 4,
    BlockFields = 5,
    Budget = 6,
    TraceAlloc = 7,
    NativePreamble = 8,
    NativeBody = 9,
    TailFetch = 10,
    TailClocks = 11,
    Refused = 12,
    InterpretFallback = 13,
    Compile = 14,
    Outliers = 15,
}

pub const N_PHASES: usize = 16;
pub const PHASE_NAMES: [&str; N_PHASES] = [
    "P0_dispatch_gates",
    "P1_key",
    "P2_probe",
    "P3_entry_guards",
    "P4_segment_layout",
    "P5_block_fields",
    "P6_budget",
    "P7_trace_alloc",
    "P8_native_preamble",
    "P9_native_body",
    "P10_tail_fetch",
    "P11_tail_clocks",
    "P12_refused",
    "P13_interpret_fallback",
    "P14_compile",
    "P15_outliers",
];

/// Four lanes on `(jit_mode_key bit 0, bit 2)` = (CS.D, V86). Index = `d | (v86 << 1)`.
pub const N_LANES: usize = 4;
pub const LANE_NAMES: [&str; N_LANES] = [
    "sixteen_bit",    // CS.D = 0, not V86 — real mode and 16-bit protected mode
    "thirty_two",     // CS.D = 1, not V86
    "v86_sixteen",    // CS.D = 0, V86
    "v86_thirty_two", // CS.D = 1, V86
];

/// The four `end()` populations. A traversal can land in more than one: a refusal takes `Refused`
/// at the early return and then `Fallback` at the interpreted arm it falls into, which is exactly
/// what A1's CUMULATIVE closures are stated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum Population {
    Entered = 0,
    Refused = 1,
    Fallback = 2,
    Compile = 3,
}
pub const N_POPULATIONS: usize = 4;
pub const POPULATION_NAMES: [&str; N_POPULATIONS] = ["entered", "refused", "fallback", "compile"];

/// §6 bins: `exit.instructions` 1..=32 (saturating) × `linked_transfers ∈ {0, 1, 2+}` × self-loop.
/// Self-loop entries are fitted separately because their `exit.instructions` is a multiple of one
/// body.
pub const N_INSN_BINS: usize = 32;
pub const N_HOP_BINS: usize = 3;
pub const N_LOOP_BINS: usize = 2;
pub const N_BINS: usize = N_INSN_BINS * N_HOP_BINS * N_LOOP_BINS;
/// Per bin: count, summed ticks, summed `exit.instructions`, summed `linked_transfers`. The
/// design's tuple was three wide; the count is the fourth because M5's fit is WEIGHTED by bin
/// counts, and a weight the export cannot see is a weight the fit cannot use.
pub const BIN_FIELDS: usize = 4;

/// The interpreted-fallback site tag (H3-R). `run.rs:820` and `run.rs:824` write it and nothing
/// else; the single `mark(P13)` inside the `None` arm is what counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum FallbackTag {
    /// Neither `Declined` nor `Skipped` fired this traversal.
    Neither = 0,
    Declined = 1,
    Skipped = 2,
}
pub const N_FALLBACK_TAGS: usize = 3;
pub const FALLBACK_TAG_NAMES: [&str; N_FALLBACK_TAGS] = ["neither", "declined", "skipped"];

pub const N_REFUSAL_SITES: usize = 30;

/// Every early return in the measured path, as `(label, run.rs line)`. The index constants in
/// `site` below are positions in THIS table; they are written out rather than generated so a
/// reader can check one against the other by eye.
pub const REFUSAL_SITES: [(&str, u32); N_REFUSAL_SITES] = [
    ("skip_native_continuations_inactive", 1241),
    ("skip_backend_or_skip_once", 1246),
    ("skip_approximate_timing", 1249),
    ("jit16_level_zero", 1371),
    ("approximate_timing", 1374),
    ("auto_admit", 1382),
    ("direct_hot_at", 1393),
    ("decline_memo_hit", 1416),
    ("key_for_phys_none", 1424),
    ("probe_interpret", 1428),
    ("probe_rejected", 1481),
    ("link_line_not_live", 1645),
    ("revalidate_none", 1648),
    ("dispatch_deferred_short", 1661),
    ("observer_or_diff_trace", 2254),
    ("interrupt_shadow", 2258),
    ("aggregate_accounting", 2262),
    ("native_fetch_trace", 2273),
    ("mode_key", 2278),
    ("x87_top", 2289),
    ("segment_layout_none", 2294),
    ("cs_layout", 2299),
    ("cpl", 2304),
    ("callout_privileged", 2381),
    ("data_segment", 2402),
    ("alignment", 2407),
    ("fetch_limit", 2419),
    ("entry_deferred_short", 2430),
    ("zero_budget", 2534),
    ("block_regenerated_none", 2566),
];

/// Index constants for `REFUSAL_SITES`, named at the call sites in `run.rs`.
pub(crate) mod site {
    pub(crate) const SKIP_NATIVE_CONTINUATIONS_INACTIVE: usize = 0;
    pub(crate) const SKIP_BACKEND_OR_SKIP_ONCE: usize = 1;
    pub(crate) const SKIP_APPROXIMATE_TIMING: usize = 2;
    pub(crate) const JIT16_LEVEL_ZERO: usize = 3;
    pub(crate) const APPROXIMATE_TIMING: usize = 4;
    pub(crate) const AUTO_ADMIT: usize = 5;
    pub(crate) const DIRECT_HOT_AT: usize = 6;
    pub(crate) const DECLINE_MEMO_HIT: usize = 7;
    pub(crate) const KEY_FOR_PHYS_NONE: usize = 8;
    pub(crate) const PROBE_INTERPRET: usize = 9;
    pub(crate) const PROBE_REJECTED: usize = 10;
    pub(crate) const LINK_LINE_NOT_LIVE: usize = 11;
    pub(crate) const REVALIDATE_NONE: usize = 12;
    pub(crate) const DISPATCH_DEFERRED_SHORT: usize = 13;
    pub(crate) const OBSERVER_OR_DIFF_TRACE: usize = 14;
    pub(crate) const INTERRUPT_SHADOW: usize = 15;
    pub(crate) const AGGREGATE_ACCOUNTING: usize = 16;
    pub(crate) const NATIVE_FETCH_TRACE: usize = 17;
    pub(crate) const MODE_KEY: usize = 18;
    pub(crate) const X87_TOP: usize = 19;
    pub(crate) const SEGMENT_LAYOUT_NONE: usize = 20;
    pub(crate) const CS_LAYOUT: usize = 21;
    pub(crate) const CPL: usize = 22;
    pub(crate) const CALLOUT_PRIVILEGED: usize = 23;
    pub(crate) const DATA_SEGMENT: usize = 24;
    pub(crate) const ALIGNMENT: usize = 25;
    pub(crate) const FETCH_LIMIT: usize = 26;
    pub(crate) const ENTRY_DEFERRED_SHORT: usize = 27;
    pub(crate) const ZERO_BUDGET: usize = 28;
    pub(crate) const BLOCK_REGENERATED_NONE: usize = 29;
}

/// The refusal sites that sit BEFORE `mark(P0)` at `run.rs:1418`. A3 states
/// `marks(P0) = decode_probes`; that identity holds only once these are added back, because a
/// traversal refused at, say, `run.rs:1241` bumped `seam_probes` and never reached the P0 mark.
/// The report prints both the literal A3 form and this exact one.
pub const PRE_P0_REFUSAL_SITES: [usize; 8] = [
    site::SKIP_NATIVE_CONTINUATIONS_INACTIVE,
    site::SKIP_BACKEND_OR_SKIP_ONCE,
    site::SKIP_APPROXIMATE_TIMING,
    site::JIT16_LEVEL_ZERO,
    site::APPROXIMATE_TIMING,
    site::AUTO_ADMIT,
    site::DIRECT_HOT_AT,
    site::DECLINE_MEMO_HIT,
];

/// The seven exits of the `BlockProbe::Compile` arm (B3-R). All seven take `mark(P14)`, never
/// `mark(P12)`; `compile_site` is what separates them.
pub const N_COMPILE_SITES: usize = 7;
pub const COMPILE_SITES: [(&str, u32); N_COMPILE_SITES] = [
    ("heat_demote", 1512),
    ("structural_reject", 1528),
    ("compile_retry", 1536),
    ("page_cover_failed", 1557),
    ("lane_install_demote", 1582),
    ("install_failed", 1589),
    ("installed_fall_through", 1633),
];
pub(crate) mod compile_site {
    pub(crate) const HEAT_DEMOTE: usize = 0;
    pub(crate) const STRUCTURAL_REJECT: usize = 1;
    pub(crate) const COMPILE_RETRY: usize = 2;
    pub(crate) const PAGE_COVER_FAILED: usize = 3;
    pub(crate) const LANE_INSTALL_DEMOTE: usize = 4;
    pub(crate) const INSTALL_FAILED: usize = 5;
    pub(crate) const INSTALLED_FALL_THROUGH: usize = 6;
}

/// Per-mark clamp (M4). A host preemption inside one P9 sample is otherwise worth ~10^6 normal
/// marks. Clamped marks add their excess to `outlier_ticks` and are REPORTED, never silently
/// folded.
///
/// **P14 is exempt** (M-R4): a cold compile that also pays `install`, the fast-map fill and an
/// arena compaction legitimately exceeds 0.3 ms, and clamping it would shed ticks
/// `jit_direct_compile_ns` keeps — manufacturing exactly the negative `P14 − compile_ns` gap A3
/// declares a falsifier.
pub const OUTLIER_TICKS: u64 = 1_000_000;

/// The observer's arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arm {
    Off,
    Full,
    Coarse,
}

impl Arm {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Arm::Off => "off",
            Arm::Full => "full",
            Arm::Coarse => "coarse",
        }
    }
}

struct Tally {
    last: u64,
    begin_stamp: u64,
    last_charge: u64,
    sampling: bool,
    seq: u64,
    lane: usize,
    fallback_tag: u8,
    ticks: [[u64; N_PHASES]; N_LANES],
    marks: [[u64; N_PHASES]; N_LANES],
    native: [[[u64; BIN_FIELDS]; N_BINS]; N_LANES],
    totals: [[u64; N_POPULATIONS]; N_LANES],
    refusal_site: [[u64; N_REFUSAL_SITES]; N_LANES],
    compile_site: [[u64; N_COMPILE_SITES]; N_LANES],
    fallback_tags: [[u64; N_FALLBACK_TAGS]; N_LANES],
    outlier_ticks: u64,
    outlier_marks: u64,
    lane_pin_mismatches: u64,
}

impl Tally {
    const fn new() -> Self {
        Self {
            last: 0,
            begin_stamp: 0,
            last_charge: 0,
            sampling: false,
            seq: 0,
            lane: 0,
            fallback_tag: FallbackTag::Neither as u8,
            ticks: [[0; N_PHASES]; N_LANES],
            marks: [[0; N_PHASES]; N_LANES],
            native: [[[0; BIN_FIELDS]; N_BINS]; N_LANES],
            totals: [[0; N_POPULATIONS]; N_LANES],
            refusal_site: [[0; N_REFUSAL_SITES]; N_LANES],
            compile_site: [[0; N_COMPILE_SITES]; N_LANES],
            fallback_tags: [[0; N_FALLBACK_TAGS]; N_LANES],
            outlier_ticks: 0,
            outlier_marks: 0,
            lane_pin_mismatches: 0,
        }
    }
}

thread_local! {
    // `const`-initialised, no `Drop`: access compiles to a segment-relative load rather than the
    // lazy-init check a non-const TLS carries (L4).
    static TALLY: UnsafeCell<Tally> = const { UnsafeCell::new(Tally::new()) };
}

/// SAFETY: the tally is thread-local, and every accessor below takes the borrow, mutates a few
/// integers and drops it before returning. Nothing in here calls guest code, allocates, or
/// re-enters an accessor, so a second `&mut` cannot exist while one is live.
#[inline(always)]
fn with<R>(f: impl FnOnce(&mut Tally) -> R) -> R {
    TALLY.with(|cell| f(unsafe { &mut *cell.get() }))
}

#[inline(always)]
fn rdtsc() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: `rdtsc` is unprivileged on every host the Direct backend runs on (it is
        // x86_64-only), reads no memory and has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // The Direct backend does not run here; this arm exists only so the crate still builds
        // with the feature selected on a non-x86_64 host.
        0
    }
}

/// Arm-time calibration and the process-global knob reads, resolved once.
struct Calibration {
    arm: Arm,
    sample_n: u64,
    /// Median of 1e5 back-to-back mark-shaped `rdtsc` pairs — the resolution floor.
    overhead_ticks: u64,
    tsc_at_arm: u64,
    instant_at_arm: Instant,
}

// The per-thread arm override, the same shape `DISP_LANES_OVERRIDE` carries in `env_gates.rs`.
// A process-global `OnceLock` cannot say what an arm counts without an env write the harness
// cannot order, and the fixtures have to pin the arm and the stride apart.
#[cfg(test)]
thread_local! {
    static ARM_OVERRIDE: std::cell::Cell<Option<Arm>> = const { std::cell::Cell::new(None) };
    static SAMPLE_OVERRIDE: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Arm the observer on THIS thread for the duration of a fixture, and drop everything the thread
/// had accumulated so the fixture starts from zero. `None` restores the ambient (env) arm.
#[cfg(test)]
pub(crate) fn arm_for_test(arm: Option<Arm>, sample_n: Option<u64>) {
    ARM_OVERRIDE.with(|cell| cell.set(arm));
    SAMPLE_OVERRIDE.with(|cell| cell.set(sample_n));
    reset_for_test();
}

#[inline(always)]
fn arm() -> Arm {
    #[cfg(test)]
    if let Some(forced) = ARM_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    calibration().arm
}

#[inline(always)]
fn sample_n() -> u64 {
    #[cfg(test)]
    if let Some(forced) = SAMPLE_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    calibration().sample_n
}

fn calibration() -> &'static Calibration {
    static CAL: std::sync::OnceLock<Calibration> = std::sync::OnceLock::new();
    CAL.get_or_init(|| {
        let arm = crate::jit::direct::entry_attribution_arm();
        let sample_n = crate::jit::direct::entry_attribution_sample();
        // A disarmed observer leg must not pay the 1e5-iteration calibration; under `cargo test`
        // it must, because the fixtures override the arm per thread and would otherwise read a
        // zero resolution floor.
        let overhead_ticks = if arm == Arm::Off && !cfg!(test) {
            0
        } else {
            calibrate()
        };
        Calibration {
            arm,
            sample_n,
            overhead_ticks,
            tsc_at_arm: rdtsc(),
            instant_at_arm: Instant::now(),
        }
    })
}

/// The resolution floor: the median cost of one `mark`, spent the way a `mark` spends it.
fn calibrate() -> u64 {
    const N: usize = 100_000;
    let mut deltas = Vec::with_capacity(N);
    let mut last = rdtsc();
    for _ in 0..N {
        let now = rdtsc();
        deltas.push(now.wrapping_sub(last));
        last = now;
    }
    deltas.sort_unstable();
    deltas[N / 2]
}

/// D0 (`run.rs:807`). Takes the sample decision for this traversal, latches the lane and writes
/// the cursor. **Accumulates nothing** (B1): the inter-entry gap — interpreted continuations,
/// run-loop break checks, device work — never lands in P0, and re-anchoring on the first sampled
/// traversal after an unsampled stretch is what keeps `SAMPLE=64` from being 64x inflated.
#[inline(always)]
pub(crate) fn begin(d: bool, v86: bool) {
    if arm() == Arm::Off {
        return;
    }
    let stride = sample_n();
    with(|t| {
        t.seq = t.seq.wrapping_add(1);
        t.sampling = stride <= 1 || t.seq % stride == 0;
        if !t.sampling {
            return;
        }
        t.lane = usize::from(d) | (usize::from(v86) << 1);
        let now = rdtsc();
        t.last = now;
        t.begin_stamp = now;
        t.last_charge = 0;
        t.fallback_tag = FallbackTag::Neither as u8;
    });
}

#[inline(always)]
fn accumulate(t: &mut Tally, phase: Phase) {
    let now = rdtsc();
    let raw = now.wrapping_sub(t.last);
    t.last = now;
    let index = phase as usize;
    let charged = if index == Phase::Compile as usize || raw <= OUTLIER_TICKS {
        raw
    } else {
        let excess = raw - OUTLIER_TICKS;
        t.outlier_ticks = t.outlier_ticks.saturating_add(excess);
        t.outlier_marks += 1;
        // P15 is the bucket the shed excess lands in, so the sixteen phases stay a partition of
        // the ticks the instrument saw and the excess is READABLE per lane rather than only as a
        // process-wide scalar.
        let outliers = Phase::Outliers as usize;
        t.ticks[t.lane][outliers] = t.ticks[t.lane][outliers].saturating_add(excess);
        t.marks[t.lane][outliers] += 1;
        OUTLIER_TICKS
    };
    t.last_charge = charged;
    t.ticks[t.lane][index] = t.ticks[t.lane][index].saturating_add(charged);
    t.marks[t.lane][index] += 1;
}

/// `mark(p)` — FULL arm only.
#[inline(always)]
pub(crate) fn mark(phase: Phase) {
    if arm() != Arm::Full {
        return;
    }
    with(|t| {
        if t.sampling {
            accumulate(t, phase);
        }
    });
}

/// `mark(p)` in both armed modes: the two native-window boundaries.
#[inline(always)]
pub(crate) fn mark_coarse(phase: Phase) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if t.sampling {
            accumulate(t, phase);
        }
    });
}

/// `end()` — the traversal's whole span against one population. A path that misses `end()` loses
/// one `total` sample and nothing else, because `begin()` re-anchors the cursor.
#[inline(always)]
pub(crate) fn end(population: Population) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if !t.sampling {
            return;
        }
        let span = rdtsc().wrapping_sub(t.begin_stamp);
        let slot = &mut t.totals[t.lane][population as usize];
        *slot = slot.saturating_add(span);
        // The site tag is committed HERE rather than where it is written, so the histogram counts
        // TRAVERSALS that reached the interpreted arm and can never drift from `marks(P13)`. A
        // traversal that reaches the arm without the dispatcher having produced either outcome
        // lands in `neither`, which is a real answer and not a hole.
        if population as usize == Population::Fallback as usize {
            let tag = t.fallback_tag as usize;
            t.fallback_tags[t.lane][tag] += 1;
            t.fallback_tag = FallbackTag::Neither as u8;
        }
    });
}

#[inline(always)]
pub(crate) fn note_refusal(site: usize) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if t.sampling {
            t.refusal_site[t.lane][site] += 1;
        }
    });
}

#[inline(always)]
pub(crate) fn note_compile_site(site: usize) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if t.sampling {
            t.compile_site[t.lane][site] += 1;
        }
    });
}

#[inline(always)]
pub(crate) fn set_fallback_tag(tag: FallbackTag) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if t.sampling {
            t.fallback_tag = tag as u8;
        }
    });
}

/// One native window into the §6 bins. `insns` is `exit.instructions` (`run.rs:2698`), `hops` is
/// `exit.linked_transfers` (`run.rs:2609`).
///
/// The tick charged is the P9 mark's own charge, read back out of the tally rather than
/// re-sampled, so the bin and the phase can never disagree and this call is not itself timed.
#[inline(always)]
pub(crate) fn note_native(insns: u64, hops: u32, self_loop: bool) {
    if arm() != Arm::Full {
        return;
    }
    with(|t| {
        if !t.sampling {
            return;
        }
        let insn_bin = (insns.clamp(1, N_INSN_BINS as u64) - 1) as usize;
        let hop_bin = hops.min(2) as usize;
        let loop_bin = usize::from(self_loop);
        let bin = (loop_bin * N_HOP_BINS + hop_bin) * N_INSN_BINS + insn_bin;
        let ticks = t.last_charge;
        let lane = t.lane;
        let row = &mut t.native[lane][bin];
        row[0] += 1;
        row[1] = row[1].saturating_add(ticks);
        row[2] = row[2].saturating_add(insns);
        row[3] += u64::from(hops);
    });
}

/// H9's pin. `run_direct_block` has no `d` parameter, so the block's own `mode_key` bit 0 is the
/// only term available there; the mode-key refusal at `run.rs:2278` already enforces the equality
/// against the live CPU for free, so a mismatch here means the LANE latched at `begin()`
/// disagrees — which is the failure H9 is about.
#[inline(always)]
pub(crate) fn pin_lane_bit0(bit: u32) {
    if arm() == Arm::Off {
        return;
    }
    with(|t| {
        if t.sampling && (t.lane & 1) as u32 != bit {
            t.lane_pin_mismatches += 1;
            debug_assert_eq!(
                (t.lane & 1) as u32,
                bit,
                "entry-attribution lane bit 0 disagrees with the block's mode key"
            );
        }
    });
}

/// Everything the exporter needs, lifted out of the thread-local in one read.
///
/// Taken on the thread that ran the guest: the tally is per-thread by construction. The headless
/// runner the design's protocol uses drives the machine on the same thread that writes the profile
/// JSON, so the snapshot is complete there. A caller on another thread gets an all-zero snapshot,
/// which the `threads_seen` field below is what makes visible rather than silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectEntryAttributionSnapshot {
    pub arm: &'static str,
    pub sample_n: u64,
    pub overhead_ticks: u64,
    pub tsc_hz: u64,
    /// `ticks[lane][phase]`, raw — no per-mark subtraction, no clamp beyond `OUTLIER_TICKS`.
    pub ticks_raw: Vec<Vec<u64>>,
    pub marks: Vec<Vec<u64>>,
    pub totals: Vec<Vec<u64>>,
    pub refusal_site: Vec<Vec<u64>>,
    pub compile_site: Vec<Vec<u64>>,
    pub fallback_tags: Vec<Vec<u64>>,
    /// `[lane][bin] = [count, ticks, insns, hops]`; empty bins are dropped by the exporter.
    pub native_bins: Vec<Vec<[u64; BIN_FIELDS]>>,
    pub outlier_ticks: u64,
    pub outlier_marks: u64,
    pub lane_pin_mismatches: u64,
}

/// Read the calling thread's tally. Returns `None` when the observer was never armed, so the
/// exporter can emit `null` and the plain build's key set stays the comparand.
pub(crate) fn snapshot() -> Option<DirectEntryAttributionSnapshot> {
    let cal = calibration();
    let live_arm = arm();
    if live_arm == Arm::Off {
        return None;
    }
    let elapsed = cal.instant_at_arm.elapsed().as_secs_f64();
    let tsc_delta = rdtsc().wrapping_sub(cal.tsc_at_arm);
    let tsc_hz = if elapsed > 0.0 {
        (tsc_delta as f64 / elapsed) as u64
    } else {
        0
    };
    Some(with(|t| DirectEntryAttributionSnapshot {
        arm: live_arm.label(),
        sample_n: sample_n(),
        overhead_ticks: cal.overhead_ticks,
        tsc_hz,
        ticks_raw: t.ticks.iter().map(|row| row.to_vec()).collect(),
        marks: t.marks.iter().map(|row| row.to_vec()).collect(),
        totals: t.totals.iter().map(|row| row.to_vec()).collect(),
        refusal_site: t.refusal_site.iter().map(|row| row.to_vec()).collect(),
        compile_site: t.compile_site.iter().map(|row| row.to_vec()).collect(),
        fallback_tags: t.fallback_tags.iter().map(|row| row.to_vec()).collect(),
        native_bins: t.native.iter().map(|row| row.to_vec()).collect(),
        outlier_ticks: t.outlier_ticks,
        outlier_marks: t.outlier_marks,
        lane_pin_mismatches: t.lane_pin_mismatches,
    }))
}

/// Test-only: drop everything the calling thread accumulated. The arm and the calibration are
/// process-global `OnceLock`s and are deliberately NOT reset — a fixture that needs a different
/// arm has to say so through the parse helpers, which are pure functions for exactly that reason.
#[cfg(test)]
pub(crate) fn reset_for_test() {
    with(|t| *t = Tally::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_and_site_tables_are_the_stated_size() {
        assert_eq!(PHASE_NAMES.len(), N_PHASES);
        assert_eq!(LANE_NAMES.len(), N_LANES);
        assert_eq!(POPULATION_NAMES.len(), N_POPULATIONS);
        assert_eq!(REFUSAL_SITES.len(), N_REFUSAL_SITES);
        assert_eq!(COMPILE_SITES.len(), N_COMPILE_SITES);
        assert_eq!(N_BINS, N_INSN_BINS * N_HOP_BINS * N_LOOP_BINS);
    }

    #[test]
    fn refusal_site_indices_name_their_own_rows() {
        // The index constants are hand-written positions into `REFUSAL_SITES`; this is the check
        // that a table edit which forgets one is caught rather than silently mis-attributed.
        assert_eq!(
            REFUSAL_SITES[site::SKIP_NATIVE_CONTINUATIONS_INACTIVE].1,
            1241
        );
        assert_eq!(REFUSAL_SITES[site::DECLINE_MEMO_HIT].1, 1416);
        assert_eq!(REFUSAL_SITES[site::KEY_FOR_PHYS_NONE].1, 1424);
        assert_eq!(REFUSAL_SITES[site::PROBE_REJECTED].1, 1481);
        assert_eq!(REFUSAL_SITES[site::SEGMENT_LAYOUT_NONE].1, 2294);
        assert_eq!(REFUSAL_SITES[site::CALLOUT_PRIVILEGED].1, 2381);
        assert_eq!(REFUSAL_SITES[site::ENTRY_DEFERRED_SHORT].1, 2430);
        assert_eq!(REFUSAL_SITES[site::BLOCK_REGENERATED_NONE].1, 2566);
        assert_eq!(COMPILE_SITES[compile_site::HEAT_DEMOTE].1, 1512);
        assert_eq!(COMPILE_SITES[compile_site::INSTALLED_FALL_THROUGH].1, 1633);
    }

    #[test]
    fn pre_p0_sites_are_exactly_the_returns_above_the_p0_mark() {
        // `mark(P0)` is at `run.rs:1418`. Every refusal site with a smaller line number must be
        // in the pre-P0 set, and no site at or after it may be.
        for (index, (label, line)) in REFUSAL_SITES.iter().enumerate() {
            let listed = PRE_P0_REFUSAL_SITES.contains(&index);
            assert_eq!(
                listed,
                *line < 1418,
                "{label} at {line} is on the wrong side"
            );
        }
    }

    #[test]
    fn native_bin_index_is_a_bijection_on_its_three_terms() {
        let mut seen = std::collections::HashSet::new();
        for loop_bin in 0..N_LOOP_BINS {
            for hop_bin in 0..N_HOP_BINS {
                for insn_bin in 0..N_INSN_BINS {
                    let bin = (loop_bin * N_HOP_BINS + hop_bin) * N_INSN_BINS + insn_bin;
                    assert!(bin < N_BINS);
                    assert!(seen.insert(bin));
                }
            }
        }
        assert_eq!(seen.len(), N_BINS);
    }

    #[test]
    fn calibration_median_is_a_plausible_mark_cost() {
        // Sanity, not a pin: one back-to-back `rdtsc` pair is tens of ticks on every host this
        // runs on. Zero would mean the timer is not advancing; a huge value would mean the median
        // picked up a preemption, which a median of 1e5 is chosen to prevent.
        let median = calibrate();
        assert!(
            median > 0,
            "rdtsc did not advance across 1e5 back-to-back reads"
        );
        assert!(
            median < 10_000,
            "calibration median {median} is not a mark cost"
        );
    }
}
