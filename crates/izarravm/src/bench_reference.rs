// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Reference targets for the Neurketa CPU and memory benchmarks.
//!
//! Every target uses the same hard acceptance window: 0.98 through 1.05 of
//! the reference value. The asymmetric window keeps the accepted center a
//! little fast, as requested for guest timing.
//!
//! Dhrystone and Whetstone carry the project reference values for the three
//! hardware classes. The 386-slow row is the 22 MHz 386 result divided by
//! three because it has the same ISA, operation costs, and 64 KiB external
//! cache at exactly one-third the clock. Sieve, fp-Mandelbrot, and the cache
//! tiers are deterministic calibration probes rather than period standards.

use izarravm_core::GswMode;

/// Dhrystones per second of the VAX 11/780 reference.
#[allow(dead_code)]
pub const VAX_DHRYSTONES_PER_SEC: f64 = 1757.0;

pub const BAND_LOW_RATIO: f64 = 0.98;
pub const BAND_HIGH_RATIO: f64 = 1.05;

/// One target in the benchmark's native comparison unit.
pub struct BenchBand {
    pub payload: &'static str,
    pub mode: GswMode,
    pub target: f64,
    pub lo: f64,
    pub hi: f64,
    #[allow(dead_code)]
    pub unit: &'static str,
    #[allow(dead_code)]
    pub cite: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandVerdict {
    InBand,
    Low,
    High,
}

impl BenchBand {
    pub fn verdict(&self, measured: f64) -> BandVerdict {
        if measured < self.lo {
            BandVerdict::Low
        } else if measured > self.hi {
            BandVerdict::High
        } else {
            BandVerdict::InBand
        }
    }
}

const fn band(
    payload: &'static str,
    mode: GswMode,
    target: f64,
    unit: &'static str,
    cite: &'static str,
) -> BenchBand {
    BenchBand {
        payload,
        mode,
        target,
        lo: target * BAND_LOW_RATIO,
        hi: target * BAND_HIGH_RATIO,
        unit,
        cite,
    }
}

pub fn band_for(payload: &str, mode: GswMode) -> Option<&'static BenchBand> {
    BENCH_BANDS
        .iter()
        .find(|entry| entry.payload == payload && entry.mode == mode)
}

/// `band_for`, keyed by the guest-clock model epoch the measuring machine ran
/// under (`Machine::timing_epoch`).
///
/// Epoch 2's whole-bill fold (slice 2) re-solves `tier_cost` on both fast
/// personas and takes `bus_timing` to `(1, 1)`, so a tier's cost in guest clocks
/// changes and the memory-bandwidth rows the old bands describe no longer exist.
/// A band that describes a model the tree no longer runs is worse than no band,
/// so the six affected rows have an epoch-2 twin here; every other payload falls
/// through to `BENCH_BANDS`.
pub fn band_for_epoch(payload: &str, mode: GswMode, epoch: u32) -> Option<&'static BenchBand> {
    if epoch >= 2
        && let Some(entry) = EPOCH2_BENCH_BANDS
            .iter()
            .find(|entry| entry.payload == payload && entry.mode == mode)
    {
        return Some(entry);
    }
    band_for(payload, mode)
}

/// The epoch-2 memory-bandwidth bands, DERIVED from the model rather than fitted
/// to a game: the sweep reads one dword per bus access, an access costs
/// `2 + tier_cost(mode, 2).<tier>` raw clocks (`BusCycle::clocks_for`), and
/// `bus_timing(persona, 2) = (1, 1)` makes a raw clock a guest clock. A tier
/// below L1 is reached once per LINE, not once per dword: the resolution installs
/// the line into L1, so a 32-byte 586 line is one L2/RAM access plus seven L1
/// hits and the per-dword cost is `(2 + ws + 7 * 2) / 8`. Each target below is
/// that arithmetic; where a tier's block sizes spread (the cold first pass costs
/// more on a bigger block), the target centres the spread so every block in the
/// tier sits inside the same 0.98-1.05 window.
///
/// RECORDED FINDING, not a fit: these sit BELOW the SpeedSys-era figures the
/// epoch-1 bands carry -- the 586 goes 2160 / 299.5 / 242.9 -> 332 / 188 / 116
/// MB/s, and the 486 198 / 50.0 / 40.5 -> 132 / 81 / 52.8 (the 486's lower tiers
/// go UP, because its epoch-1 `tier_cost` priced a miss at 191/250 wait states).
/// The L1 shortfall is structural and predates this slice: the model charges one
/// non-burst 2-clock bus cycle per dword and has no line fill, so L1 bandwidth
/// cannot exceed `2 * clock_hz` bytes/s whatever the wait state is. Epoch 1 hid
/// that behind `bus_timing`'s 0.152 multiplier -- exactly the dial the fold
/// removes. Restoring era L1 bandwidth needs a burst/line-fill model, which is
/// nobody's slice yet.
pub const EPOCH2_BENCH_BANDS: &[BenchBand] = &[
    band(
        "bandwidth-l1",
        GswMode::Gsw486,
        132.0,
        "MB/s",
        "Epoch 2, derived: 4 bytes per (2 + 0) clocks at 66 MHz -- the bus-cycle floor, since the 486 L1 wait state is 0 under the whole-bill fold",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw486,
        80.9,
        "MB/s",
        "Epoch 2, derived: a 16-byte line is one L2 access (2 + 5) plus three L1 hits, 13 clocks per 16 bytes at 66 MHz = 81.2 steady state; 80.9 centres the tier's cold-pass spread",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw486,
        52.8,
        "MB/s",
        "Epoch 2, derived: a 16-byte line is one RAM access (2 + 12) plus three L1 hits, 20 clocks per 16 bytes at 66 MHz",
    ),
    band(
        "bandwidth-l1",
        GswMode::Gsw586,
        332.0,
        "MB/s",
        "Epoch 2, derived: 4 bytes per (2 + 0) clocks at 166 MHz -- the 2-clock bus-cycle floor, not an L1 figure; see the module note",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw586,
        187.9,
        "MB/s",
        "Epoch 2, derived: a 32-byte line is one L2 access (2 + 12) plus seven L1 hits, 28 clocks per 32 bytes at 166 MHz = 189.7 steady state; 187.9 centres the tier's cold-pass spread",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw586,
        115.5,
        "MB/s",
        "Epoch 2, derived: a 32-byte line is one RAM access (2 + 30) plus seven L1 hits, 46 clocks per 32 bytes at 166 MHz",
    ),
];

pub const BENCH_BANDS: &[BenchBand] = &[
    band(
        "dhrystone",
        GswMode::Gsw386Slow,
        9200.0 / 3.0,
        "iters/sec",
        "386DX-22 project target divided by three for the identical 386-slow machine",
    ),
    band(
        "dhrystone",
        GswMode::Gsw386,
        9200.0,
        "iters/sec",
        "Project reference: 386DX at 22 MHz, about 9200 Dhrystones/sec",
    ),
    band(
        "dhrystone",
        GswMode::Gsw486,
        61_000.0,
        "iters/sec",
        "Project reference: 486DX2 at 66 MHz, about 61000 Dhrystones/sec",
    ),
    band(
        "dhrystone",
        GswMode::Gsw586,
        337_200.0,
        "iters/sec",
        // THE one Dhrystone-586 number in the tree. `izarravm-cpu`'s `level_timing`
        // comment carried a stale ~250,000 beside it until slice 1f; that value
        // matched neither this band nor the ~337,000 an era Pentium 166 scores at
        // ~190 DMIPS, and it is gone. NOT A TARGET: the owner demoted Dhrystone
        // from the recalibration's grading entirely (12:10, 2026-09-03), behind
        // quake's demo time and doom's realtics window. It stays as a recorded
        // reference and as the Neurketa harness's own band, and the epoch-2 class
        // tables are EXPECTED to move it on both fast personas.
        "Recorded reference, not a target: GSW-586 at 166 MHz on the PC100 board, about 337,200          Dhrystones/sec",
    ),
    band(
        "sieve",
        GswMode::Gsw386Slow,
        49.6 / 3.0,
        "iters/sec",
        "Deterministic BYTE Sieve calibration derived from the identical 386 row",
    ),
    band(
        "sieve",
        GswMode::Gsw386,
        49.6,
        "iters/sec",
        "Deterministic BYTE Sieve calibration for the 386DX-22 profile",
    ),
    band(
        "sieve",
        GswMode::Gsw486,
        372.0,
        "iters/sec",
        "Deterministic BYTE Sieve calibration for the 486DX2-66 profile",
    ),
    band(
        "sieve",
        GswMode::Gsw586,
        1813.0,
        "iters/sec",
        "Deterministic BYTE Sieve calibration for the GSW-586 166 MHz / PC100 profile",
    ),
    band(
        "fp-mandel",
        GswMode::Gsw486,
        24_600.0,
        "iters/sec",
        "Deterministic x87 Mandelbrot calibration; Whetstone remains the period FP reference",
    ),
    band(
        "fp-mandel",
        GswMode::Gsw586,
        145_200.0,
        "iters/sec",
        "Deterministic x87 Mandelbrot calibration; Whetstone remains the period FP reference",
    ),
    band(
        "whetstone",
        GswMode::Gsw486,
        6.5,
        "MFLOPS",
        "Project reference: 486DX2-66 at about 6.5 Whetstone MFLOPS",
    ),
    band(
        "whetstone",
        GswMode::Gsw586,
        36.8,
        "MFLOPS",
        "Project reference: GSW-586 166 MHz / PC100 at about 36.8 Whetstone MFLOPS; the PC100 bus recalibration lifts the x87 memory traffic above the plain-166 28.6",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw386Slow,
        59.3 / 3.0,
        "MB/s",
        "386 external-cache calibration divided by three for 386-slow",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw386Slow,
        54.2 / 3.0,
        "MB/s",
        "386 RAM calibration divided by three for 386-slow",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw386,
        59.3,
        "MB/s",
        "Deterministic 64 KiB external-cache calibration for the 386DX-22 profile",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw386,
        54.2,
        "MB/s",
        "Deterministic RAM calibration for the 386DX-22 profile",
    ),
    band(
        "bandwidth-l1",
        GswMode::Gsw486,
        197.8,
        "MB/s",
        "Deterministic 8 KiB L1 calibration for the 486DX2-66 profile",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw486,
        50.0,
        "MB/s",
        "486DX2-66 external-cache target, anchored to period SpeedSys results",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw486,
        40.5,
        "MB/s",
        "486DX2-66 RAM target, anchored to period SpeedSys results",
    ),
    band(
        "bandwidth-l1",
        GswMode::Gsw586,
        2160.0,
        "MB/s",
        "Deterministic split-L1 calibration for the GSW-586 166 MHz / PC100 profile",
    ),
    band(
        "bandwidth-l2",
        GswMode::Gsw586,
        299.5,
        "MB/s",
        "Deterministic 512 KiB L2 calibration for the GSW-586 166 MHz / PC100 profile",
    ),
    band(
        "bandwidth-ram",
        GswMode::Gsw586,
        242.9,
        "MB/s",
        "Deterministic PC100 SDRAM calibration for the GSW-586 166 MHz profile",
    ),
];

#[cfg(test)]
#[path = "bench_reference_test.rs"]
mod tests;
