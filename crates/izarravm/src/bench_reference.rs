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
        "Project reference: GSW-586 at 166 MHz on the PC100 board, about 337000 Dhrystones/sec",
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
