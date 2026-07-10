// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// All four modes, slowest to fastest, for relative-width checks.
const MODES: [GswMode; 4] = [
    GswMode::Gsw386Slow,
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
        // bandwidth tiers stay MB/s for the probe task; Whetstone bands in MFLOPS.
        let want_unit = if band.payload.starts_with("bandwidth-") {
            "MB/s"
        } else if band.payload == "whetstone" {
            "MFLOPS"
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
fn dhrystone_586_is_the_owner_authoritative_target() {
    // The 586 Dhrystone band is the project owner's AUTHORITATIVE target for the
    // P55C retarget (~300000 Dhrystones/sec, ~170.5 DMIPS, Pentium MMX-200 @ 200
    // MHz), hit by the bus_timing(I586) re-tune. The cite must mark it
    // owner-authoritative, and the target's DMIPS must be in the documented ~170.5
    // neighborhood. VAX_DHRYSTONES_PER_SEC stays the DMIPS conversion.
    let band = band_for("dhrystone", GswMode::Gsw586).expect("586 dhrystone band");
    assert!(
        band.cite.contains("OWNER AUTHORITATIVE"),
        "586 dhrystone cite must mark the owner-authoritative target: {}",
        band.cite
    );
    assert!(
        (band.target - 300_000.0).abs() < 1.0,
        "586 dhrystone target {} must be the owner's 300000 (P55C)",
        band.target
    );
    let dmips = band.target / VAX_DHRYSTONES_PER_SEC;
    assert!(
        (160.0..180.0).contains(&dmips),
        "586 target {} is {dmips:.1} DMIPS, expected ~170.5 (owner authoritative)",
        band.target
    );
}

#[test]
fn whetstone_bands_are_the_owner_authoritative_fp_targets() {
    // The Whetstone bands carry the owner's authoritative FP targets in MFLOPS:
    // 486DX2-66 = 6.5, Pentium MMX-200 = 34.5. 486 anchors the FLOP weight;
    // fp_timing(I586) seats the 586. Not encoded for the FPU-less 286/386.
    let w486 = band_for("whetstone", GswMode::Gsw486).expect("486 whetstone band");
    let w586 = band_for("whetstone", GswMode::Gsw586).expect("586 whetstone band");
    assert!(
        (w486.target - 6.5).abs() < 0.01,
        "486 whetstone target {}",
        w486.target
    );
    assert!(
        (w586.target - 34.5).abs() < 0.01,
        "586 whetstone target {}",
        w586.target
    );
    assert_eq!(w486.unit, "MFLOPS");
    assert_eq!(w586.unit, "MFLOPS");
    assert!(w486.cite.contains("OWNER AUTHORITATIVE"));
    assert!(w586.cite.contains("OWNER AUTHORITATIVE"));
    assert!(band_for("whetstone", GswMode::Gsw386Slow).is_none());
    assert!(band_for("whetstone", GswMode::Gsw386).is_none());
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
        band_for("fp-mandel", GswMode::Gsw386Slow).is_none(),
        "fp-mandel must not be encoded for the FPU-less 286"
    );
    assert!(
        band_for("fp-mandel", GswMode::Gsw386).is_none(),
        "fp-mandel must not be encoded for the FPU-less 386"
    );
    // Bandwidth tiers per the cache geometry: 286 RAM only; 386 L2 + RAM;
    // 486 and 586 L1 + L2 + RAM.
    assert!(band_for("bandwidth-ram", GswMode::Gsw386Slow).is_some());
    assert!(band_for("bandwidth-l1", GswMode::Gsw386Slow).is_none());
    assert!(band_for("bandwidth-l2", GswMode::Gsw386Slow).is_none());

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
fn bench_band_tiers_descend_l1_l2_ram() {
    // The "L1 > L2 > RAM" descending invariant is documented in the cites; this
    // enforces it on the encoded targets for every mode that has the tiers.
    // (286 has RAM only -- its presence is covered by the presence test.)
    let target = |tier, mode| {
        band_for(tier, mode)
            .unwrap_or_else(|| panic!("missing {tier} band for {mode:?}"))
            .target
    };
    // 486 and 586: all three tiers present, l1 > l2 > ram.
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let l1 = target("bandwidth-l1", mode);
        let l2 = target("bandwidth-l2", mode);
        let ram = target("bandwidth-ram", mode);
        assert!(
            l1 > l2,
            "{mode:?}: L1 target {l1} must exceed L2 target {l2}"
        );
        assert!(
            l2 > ram,
            "{mode:?}: L2 target {l2} must exceed RAM target {ram}"
        );
    }
    // 386: no L1; l2 > ram.
    let l2_386 = target("bandwidth-l2", GswMode::Gsw386);
    let ram_386 = target("bandwidth-ram", GswMode::Gsw386);
    assert!(
        l2_386 > ram_386,
        "386: L2 target {l2_386} must exceed RAM target {ram_386}"
    );
}

#[test]
fn bench_band_in_order_modes_are_tighter_than_586() {
    // For each runnable payload, every in-order mode's relative band width
    // must be <= the 586's (the superscalar mode carries the widest gap).
    for payload in ["dhrystone", "sieve", "fp-mandel", "whetstone"] {
        let Some(superscalar) = band_for(payload, GswMode::Gsw586) else {
            continue;
        };
        let wide = relative_width(superscalar);
        for mode in [GswMode::Gsw386Slow, GswMode::Gsw386, GswMode::Gsw486] {
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

#[test]
fn approximate_mode_bands_are_loose_accurate_mode_bands_are_tight() {
    for payload in ["dhrystone", "sieve", "fp-mandel", "whetstone"] {
        for mode in MODES {
            let Some(band) = band_for(payload, mode) else {
                continue;
            };
            let width = relative_width(band);
            if mode.uses_approximate_timing() {
                assert!(
                    width >= 0.30,
                    "{payload} {mode:?} approximate band width {width} must be loose (>=0.30)"
                );
            } else {
                assert!(
                    width <= 0.22,
                    "{payload} {mode:?} accurate band width {width} must stay tight (<=0.22)"
                );
            }
        }
    }
}
