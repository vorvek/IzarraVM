//! Per-mode era reference bands for the Neurketa benchmarks.
//!
//! These are the TARGET numbers each CPU mode should compute, sourced from
//! published period benchmark data and scaled to the IZARRA clock rates (286 @
//! 8.33 MHz, 386 @ 22 MHz, 486 DX2 @ 66 MHz, K6 "Chomper" 586 @ 266 MHz). They
//! are the success oracle the later calibration tasks tune against; today the
//! per-mode timing is mis-calibrated, so `--headless-bench` rows tag mostly LOW
//! or HIGH against these bands. That is expected until the cache model and the
//! instruction-clock scalar are calibrated.
//!
//! Every band is encoded in the bench's NATIVE comparison unit (`iters/sec`) so
//! the reporter compares directly:
//! - Dhrystone: one iteration == one "Dhrystone", so `iters/sec` ==
//!   Dhrystones/sec. The classic DMIPS unit = Dhrystones/sec / 1757 (the VAX
//!   11/780 reference), so a DMIPS figure is encoded here as `DMIPS * 1757`.
//! - Sieve: `iters/sec` == sieve passes/sec (the classic 8190-element BYTE
//!   Sieve; the bench reports 40 passes per run, 1899 primes).
//! - fp-Mandelbrot: `iters/sec` == pixels/sec (48x32 = 1536 pixels per run,
//!   maxiter 64). 486/586 only (no FPU below 486).
//! - Bandwidth tiers: kept in MB/s (unit "MB/s"); a later probe task consumes
//!   these, there is no bench row for them yet.
//!
//! Bands are graduated: +/-5% by default; +/-10% for the 286 (sparse period
//! data); a wide best-effort band for the superscalar 586 (record the gap, do
//! not over-tighten); fp-mandel a wider band with a documented basis. All
//! targets are biased HIGH within their plausible range: the Izarra board gives
//! every mode a 66 MHz FSB plus SDRAM the historical low-end parts lacked.
//!
//! Sources (cross-checked where possible; see the `cite` on each entry):
//! - Roy Longbottom, "Dhrystone Benchmark Results" (PC collection): AMD K6 @ 200
//!   MHz = 349 VAX MIPS (Dhry1 Opt) / 289 (Dhry2 Opt); 80486 DX2-66 = 45.1
//!   (Dhry1 Opt) / 35.3 (Dhry2 Opt); Am386DX-40 = 17.5 / 13.7.
//! - netlib "dhrystone.data" tables: 486DX2/66 MS-DOS 6.2 = 36.9 (V1.1) / 31.4
//!   (V2.1) VAX MIPS.
//! - Wikipedia "Instructions per second": 80286 1.28 DMIPS @ 12 MHz; i386DX 2.15
//!   DMIPS @ 16 MHz; i486DX2 25.6 DMIPS @ 66 MHz.
//! - DOS Days "A 286 Running Like a 386?": Check-It 3 286 Dhrystones/sec, 3157
//!   (16 MHz, 1 WS) to 6374 (25 MHz, 0 WS); 386DX-33 + 256K L2 = 8500,
//!   Am386DX-40 = 11947 Dhrystones/sec.
//! - "The (Almost) Definitive 486DX/50 and DX2/66" (SpeedSys): 486DX2-66 L1 read
//!   62.3 MB/s; period 486 RAM bandwidth ~38 MB/s.
//! - VOGONS SpeedSys reports: K6-2(+) class memory bandwidth ~244 MB/s with L2;
//!   Super-7 K6 L1 read several hundred MB/s.
//! - Roy Longbottom Whetstone: 486DX2-66 ~15.3 MWIPS; AMD K6 @ 200 ~47.7 MFLOPS
//!   (the fp ratio that anchors fp-mandel 586 vs 486, ~4x).

use izarravm_core::GswMode;

/// The Dhrystones-per-second the VAX 11/780 reference managed, used to convert
/// between published DMIPS / VAX MIPS figures and the bench's Dhrystones/sec.
/// Read by the band test; the later calibration tasks use it to convert DMIPS
/// targets, so it is allowed dead in the binary build until then.
#[allow(dead_code)]
pub const VAX_DHRYSTONES_PER_SEC: f64 = 1757.0;

/// One era-reference band: a target value (biased high) and an acceptance window
/// in the bench's native comparison unit, plus a short source note.
pub struct BenchBand {
    /// "dhrystone" | "sieve" | "fp-mandel" | "bandwidth-l1" | "bandwidth-l2" |
    /// "bandwidth-ram".
    pub payload: &'static str,
    pub mode: GswMode,
    /// Era-correct value, biased high.
    pub target: f64,
    pub lo: f64,
    pub hi: f64,
    /// "iters/sec" for the runnable benches (so the reporter compares directly);
    /// "MB/s" for the bandwidth tiers consumed by the probe task. `unit` and
    /// `cite` are read by the band test and the deferred probe/report tasks, not
    /// the current reporter, so they are allowed dead in the binary build.
    #[allow(dead_code)]
    pub unit: &'static str,
    /// Short source note for the number.
    #[allow(dead_code)]
    pub cite: &'static str,
}

/// The result of comparing a measured `iters/sec` to a band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandVerdict {
    InBand,
    /// Below `lo`; carries the measured/target ratio (< 1.0).
    Low,
    /// Above `hi`; carries the measured/target ratio (> 1.0).
    High,
}

impl BenchBand {
    /// Classify a measured value against this band.
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

/// Look up the band for a payload running in a mode, if one is encoded.
pub fn band_for(payload: &str, mode: GswMode) -> Option<&'static BenchBand> {
    BENCH_BANDS
        .iter()
        .find(|band| band.payload == payload && band.mode == mode)
}

// Helpers used only to keep the table literals readable. A `const fn` cannot
// call closures or floating multiply in a const initializer cleanly across all
// toolchains we target, so the table writes the final iters/sec directly and the
// doc comment above records the DMIPS basis for each Dhrystone row.

/// Era reference bands, one entry per (payload, applicable mode).
///
/// Dhrystone targets in Dhrystones/sec (== iters/sec) with the DMIPS basis noted:
/// - 286 @ 8.33: ~1.25 DMIPS -> ~2200/s (Check-It 3 25 MHz 0-WS scaled, high).
/// - 386 @ 22:   ~3.70 DMIPS -> ~6500/s (Am386DX-40 / 386DX-33+L2 scaled, cached).
/// - 486 @ 66:   ~34.0 DMIPS -> ~59700/s (Roy Longbottom Dhry2 Opt 35.3, high).
/// - 586 @ 266:  ~460 DMIPS  -> ~808000/s (K6@200 1.745 DMIPS/MHz; owner anchor 455).
pub const BENCH_BANDS: &[BenchBand] = &[
    // ---- Dhrystone (all four modes); target = Dhrystones/sec. ----
    BenchBand {
        payload: "dhrystone",
        mode: GswMode::Gsw286,
        target: 2200.0,
        lo: 1980.0, // +/-10%: sparse 286 period data.
        hi: 2420.0,
        unit: "iters/sec",
        cite: "Check-It 3 286: 6374 D/s @25MHz 0WS (~255 D/s/MHz) x 8.33, biased high; ~1.25 DMIPS",
    },
    BenchBand {
        payload: "dhrystone",
        mode: GswMode::Gsw386,
        target: 6500.0,
        lo: 6175.0, // +/-5%.
        hi: 6825.0,
        unit: "iters/sec",
        cite: "Am386DX-40 11947 D/s (~299 D/s/MHz) & 386DX-33+256K L2 8500 D/s, scaled to 22MHz; ~3.7 DMIPS",
    },
    BenchBand {
        payload: "dhrystone",
        mode: GswMode::Gsw486,
        target: 59700.0,
        lo: 56715.0, // +/-5%.
        hi: 62685.0,
        unit: "iters/sec",
        cite: "Roy Longbottom 486DX2-66 Dhry2 Opt 35.3 VAX MIPS (x1757), biased high vs netlib 31.4 / Wikipedia 25.6",
    },
    BenchBand {
        payload: "dhrystone",
        mode: GswMode::Gsw586,
        // 460 DMIPS x 1757 ~= 808220 Dhrystones/sec; owner anchor 455 DMIPS.
        target: 808220.0,
        // Best-effort wide band (the superscalar K6 carries the widest gap, so it
        // is wider than the in-order modes incl. the 286's +/-10%). The owner's
        // 440-470 DMIPS (773k-826k/s) sits comfortably inside this ~ +/-12%.
        lo: 711230.0, // ~405 DMIPS.
        hi: 905210.0, // ~515 DMIPS.
        unit: "iters/sec",
        cite: "K6@200 Dhry1 Opt 349 VAX MIPS (1.745 DMIPS/MHz) x 266 = 464; owner anchor 455, band 440-470",
    },
    // ---- BYTE Sieve (all four modes); target = sieve passes/sec. ----
    // Anchored to the period 8086 @ 8 MHz C Sieve (10 passes in 2.8 s ->
    // ~3.57 passes/s ~ 0.33 DMIPS), scaled by each mode's integer throughput.
    // The Sieve is byte-array memory bound, so the fast modes pull ahead less
    // than their full Dhrystone ratio (the cache dilution the calibration fixes).
    BenchBand {
        payload: "sieve",
        mode: GswMode::Gsw286,
        target: 13.0,
        lo: 11.7, // +/-10%: sparse 286 period data.
        hi: 14.3,
        unit: "iters/sec",
        cite: "8086@8MHz C Sieve 3.57 passes/s scaled by 286 integer ratio (~1.25 DMIPS), biased high",
    },
    BenchBand {
        payload: "sieve",
        mode: GswMode::Gsw386,
        target: 55.0,
        lo: 52.25, // +/-5%.
        hi: 57.75,
        unit: "iters/sec",
        cite: "8086 C Sieve baseline scaled by 386 integer ratio (~3.7 DMIPS, cached board), biased high",
    },
    BenchBand {
        payload: "sieve",
        mode: GswMode::Gsw486,
        target: 520.0,
        lo: 494.0, // +/-5%.
        hi: 546.0,
        unit: "iters/sec",
        cite: "8086 C Sieve baseline scaled by 486DX2-66 integer ratio (~34 DMIPS), biased high",
    },
    BenchBand {
        payload: "sieve",
        mode: GswMode::Gsw586,
        target: 5500.0,
        lo: 4675.0, // best-effort wide band (superscalar K6; memory-bound dampens IPC).
        hi: 6050.0,
        unit: "iters/sec",
        cite: "8086 C Sieve baseline scaled by K6@266 integer ratio, memory-bound dampened; wide 586 band",
    },
    // ---- fp-Mandelbrot (486/586 only); target = pixels/sec. ----
    // No standardized period table for this exact payload, so it is anchored to
    // the 486/586 x87 FPU throughput ratio implied by published Whetstone:
    // 486DX2-66 ~15.3 MWIPS vs K6@266 ~63.5 MWIPS (47.7 MFLOPS @200 scaled) =>
    // ~4.15x. The 486 absolute is set from its integer-class throughput tempered
    // by x87 latency (an iteration is several x87 ops over maxiter 64); the 586
    // is ~4.15x the 486. Wider bands because the absolute basis is a ratio, not a
    // published table.
    BenchBand {
        payload: "fp-mandel",
        mode: GswMode::Gsw486,
        target: 9000.0,
        lo: 7650.0, // +/-15%: ratio-anchored, no period absolute.
        hi: 10350.0,
        unit: "iters/sec",
        cite: "486DX2-66 x87, anchored to ~15.3 MWIPS Whetstone over a maxiter-64 mandel pixel; +/-15% (no period table)",
    },
    BenchBand {
        payload: "fp-mandel",
        mode: GswMode::Gsw586,
        // ~4.15x the 486 fp throughput (Whetstone K6@266 / 486DX2-66).
        target: 37000.0,
        lo: 29600.0, // ~ -20%: best-effort wide 586 band on a ratio-anchored absolute.
        hi: 44400.0, // ~ +20%.
        unit: "iters/sec",
        cite: "K6@266 ~63.5 MWIPS / 486DX2-66 ~15.3 MWIPS ~= 4.15x the 486 target; wide ratio-anchored band",
    },
    // ---- Memory read bandwidth tiers (MB/s), consumed by the later probe task. ----
    // 286: RAM only (no cache). 386: L2 + RAM. 486/586: L1 + L2 + RAM. Biased
    // high (the Izarra board's 66 MHz FSB + SDRAM). Period anchors: 486DX2-66
    // SpeedSys L1 read 62.3 MB/s, period 486 RAM ~38 MB/s; K6-2(+) memory ~244
    // MB/s with L2, Super-7 L1 several hundred MB/s.
    BenchBand {
        payload: "bandwidth-ram",
        mode: GswMode::Gsw286,
        target: 16.0,
        lo: 14.4, // +/-10%: sparse 286 data.
        hi: 17.6,
        unit: "MB/s",
        cite: "286 RAM-only; biased high vs ~6-12 MB/s period 286 DRAM, Izarra fast FSB",
    },
    BenchBand {
        payload: "bandwidth-l2",
        mode: GswMode::Gsw386,
        target: 60.0,
        lo: 57.0, // +/-5%.
        hi: 63.0,
        unit: "MB/s",
        cite: "386 L2 (motherboard cache) read; biased high, near a 486's L2 tier on the Izarra board",
    },
    BenchBand {
        payload: "bandwidth-ram",
        mode: GswMode::Gsw386,
        target: 40.0,
        lo: 38.0, // +/-5%.
        hi: 42.0,
        unit: "MB/s",
        cite: "386 RAM; period 486-era RAM ~38 MB/s (SpeedSys), biased high for the Izarra SDRAM",
    },
    BenchBand {
        payload: "bandwidth-l1",
        mode: GswMode::Gsw486,
        target: 70.0,
        lo: 66.5, // +/-5%.
        hi: 73.5,
        unit: "MB/s",
        cite: "486DX2-66 SpeedSys L1 read 62.3 MB/s, biased high",
    },
    BenchBand {
        payload: "bandwidth-l2",
        mode: GswMode::Gsw486,
        target: 50.0,
        lo: 47.5, // +/-5%.
        hi: 52.5,
        unit: "MB/s",
        cite: "486DX2-66 L2 read, below L1 and above RAM (SpeedSys 486 tiering), biased high",
    },
    BenchBand {
        payload: "bandwidth-ram",
        mode: GswMode::Gsw486,
        target: 40.0,
        lo: 38.0, // +/-5%.
        hi: 42.0,
        unit: "MB/s",
        cite: "486DX2-66 RAM ~38 MB/s (SpeedSys), biased high",
    },
    BenchBand {
        payload: "bandwidth-l1",
        mode: GswMode::Gsw586,
        target: 700.0,
        lo: 595.0, // best-effort wide band (Super-7 L1 read varies widely).
        hi: 770.0,
        unit: "MB/s",
        cite: "K6 64KB L1 (32I+32D) read; Super-7 SpeedSys L1 several hundred MB/s, biased high; wide 586 band",
    },
    BenchBand {
        payload: "bandwidth-l2",
        mode: GswMode::Gsw586,
        target: 244.0,
        lo: 207.0, // best-effort wide band.
        hi: 268.0,
        unit: "MB/s",
        cite: "K6-2(+) SpeedSys memory ~244 MB/s with 128K L2 (VOGONS), biased high; wide 586 band",
    },
    BenchBand {
        payload: "bandwidth-ram",
        mode: GswMode::Gsw586,
        target: 120.0,
        lo: 102.0, // best-effort wide band.
        hi: 132.0,
        unit: "MB/s",
        cite: "K6 Super-7 100 MHz SDRAM main-memory read; biased high; wide 586 band",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// All four modes, slowest to fastest, for relative-width checks.
    const MODES: [GswMode; 4] = [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ];

    fn relative_width(band: &BenchBand) -> f64 {
        (band.hi - band.lo) / band.target
    }

    #[test]
    fn bench_band_entries_are_internally_sane() {
        for band in BENCH_BANDS {
            assert!(
                band.lo <= band.target,
                "{} {:?}: lo {} > target {}",
                band.payload,
                band.mode,
                band.lo,
                band.target
            );
            assert!(
                band.target <= band.hi,
                "{} {:?}: target {} > hi {}",
                band.payload,
                band.mode,
                band.target,
                band.hi
            );
            assert!(
                band.lo > 0.0,
                "{} {:?}: non-positive lo",
                band.payload,
                band.mode
            );
            assert!(
                !band.cite.is_empty(),
                "{} {:?}: missing citation",
                band.payload,
                band.mode
            );
            // The runnable benches must compare in the bench's native unit; the
            // bandwidth tiers stay MB/s for the probe task.
            let want_unit = if band.payload.starts_with("bandwidth-") {
                "MB/s"
            } else {
                "iters/sec"
            };
            assert_eq!(
                band.unit, want_unit,
                "{} {:?}: unit {} should be {want_unit}",
                band.payload, band.mode, band.unit
            );
        }
    }

    #[test]
    fn dhrystone_target_matches_dmips_basis() {
        // The 586 Dhrystone target is the owner's ~460 DMIPS anchor expressed in
        // Dhrystones/sec, so target / VAX_DHRYSTONES_PER_SEC must land near 460.
        let band = band_for("dhrystone", GswMode::Gsw586).expect("586 dhrystone band");
        let dmips = band.target / VAX_DHRYSTONES_PER_SEC;
        assert!(
            (450.0..=470.0).contains(&dmips),
            "586 dhrystone target {} is {dmips:.1} DMIPS, expected ~460",
            band.target
        );
    }

    #[test]
    fn bench_band_every_applicable_pair_present() {
        // Dhrystone and Sieve run in all four modes.
        for payload in ["dhrystone", "sieve"] {
            for mode in MODES {
                assert!(
                    band_for(payload, mode).is_some(),
                    "missing band for {payload} {mode:?}"
                );
            }
        }
        // fp-mandel needs an FPU: 486 and 586 only (and NOT 286/386).
        assert!(band_for("fp-mandel", GswMode::Gsw486).is_some());
        assert!(band_for("fp-mandel", GswMode::Gsw586).is_some());
        assert!(
            band_for("fp-mandel", GswMode::Gsw286).is_none(),
            "fp-mandel must not be encoded for the FPU-less 286"
        );
        assert!(
            band_for("fp-mandel", GswMode::Gsw386).is_none(),
            "fp-mandel must not be encoded for the FPU-less 386"
        );
        // Bandwidth tiers per the cache geometry: 286 RAM only; 386 L2 + RAM;
        // 486 and 586 L1 + L2 + RAM.
        assert!(band_for("bandwidth-ram", GswMode::Gsw286).is_some());
        assert!(band_for("bandwidth-l1", GswMode::Gsw286).is_none());
        assert!(band_for("bandwidth-l2", GswMode::Gsw286).is_none());

        assert!(band_for("bandwidth-l2", GswMode::Gsw386).is_some());
        assert!(band_for("bandwidth-ram", GswMode::Gsw386).is_some());
        assert!(band_for("bandwidth-l1", GswMode::Gsw386).is_none());

        for mode in [GswMode::Gsw486, GswMode::Gsw586] {
            for tier in ["bandwidth-l1", "bandwidth-l2", "bandwidth-ram"] {
                assert!(
                    band_for(tier, mode).is_some(),
                    "missing {tier} for {mode:?}"
                );
            }
        }
    }

    #[test]
    fn bench_band_in_order_modes_are_tighter_than_586() {
        // For each runnable payload, every in-order mode's relative band width
        // must be <= the 586's (the superscalar mode carries the widest gap).
        for payload in ["dhrystone", "sieve", "fp-mandel"] {
            let Some(superscalar) = band_for(payload, GswMode::Gsw586) else {
                continue;
            };
            let wide = relative_width(superscalar);
            for mode in [GswMode::Gsw286, GswMode::Gsw386, GswMode::Gsw486] {
                if let Some(band) = band_for(payload, mode) {
                    assert!(
                        relative_width(band) <= wide + f64::EPSILON,
                        "{payload} {mode:?} band width {} exceeds 586 width {wide}",
                        relative_width(band)
                    );
                }
            }
        }
    }

    #[test]
    fn bench_band_verdict_classifies() {
        let band = band_for("dhrystone", GswMode::Gsw586).expect("586 dhrystone band");
        assert_eq!(band.verdict(band.target), BandVerdict::InBand);
        assert_eq!(band.verdict(band.lo - 1.0), BandVerdict::Low);
        assert_eq!(band.verdict(band.hi + 1.0), BandVerdict::High);
    }
}
