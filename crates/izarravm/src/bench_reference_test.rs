// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const MODES: [GswMode; 4] = [
    GswMode::Gsw386Slow,
    GswMode::Gsw386,
    GswMode::Gsw486,
    GswMode::Gsw586,
];

fn target(payload: &str, mode: GswMode) -> f64 {
    band_for(payload, mode)
        .unwrap_or_else(|| panic!("missing {payload} band for {mode:?}"))
        .target
}

#[test]
fn every_band_uses_the_hard_fast_biased_window() {
    for entry in BENCH_BANDS {
        assert!((entry.lo / entry.target - BAND_LOW_RATIO).abs() < 1e-12);
        assert!((entry.hi / entry.target - BAND_HIGH_RATIO).abs() < 1e-12);
        assert!(entry.target > 0.0);
        assert!(!entry.cite.is_empty());
        let expected_unit = if entry.payload.starts_with("bandwidth-") {
            "MB/s"
        } else if entry.payload == "whetstone" {
            "MFLOPS"
        } else {
            "iters/sec"
        };
        assert_eq!(entry.unit, expected_unit);
    }
}

#[test]
fn every_applicable_mode_and_cache_tier_has_a_band() {
    for payload in ["dhrystone", "sieve"] {
        for mode in MODES {
            assert!(band_for(payload, mode).is_some(), "{payload} {mode:?}");
        }
    }
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        assert!(band_for("fp-mandel", mode).is_some());
        assert!(band_for("whetstone", mode).is_some());
        for tier in ["bandwidth-l1", "bandwidth-l2", "bandwidth-ram"] {
            assert!(band_for(tier, mode).is_some(), "{tier} {mode:?}");
        }
    }
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        assert!(band_for("fp-mandel", mode).is_none());
        assert!(band_for("whetstone", mode).is_none());
        assert!(band_for("bandwidth-l1", mode).is_none());
        assert!(band_for("bandwidth-l2", mode).is_some());
        assert!(band_for("bandwidth-ram", mode).is_some());
    }
}

#[test]
fn slow_386_targets_are_exactly_one_third_of_386() {
    for payload in ["dhrystone", "sieve", "bandwidth-l2", "bandwidth-ram"] {
        let slow = target(payload, GswMode::Gsw386Slow);
        let normal = target(payload, GswMode::Gsw386);
        assert!((slow * 3.0 - normal).abs() < 1e-9, "{payload}");
    }
}

#[test]
fn hardware_reference_targets_stay_pinned() {
    assert_eq!(target("dhrystone", GswMode::Gsw386), 9200.0);
    assert_eq!(target("dhrystone", GswMode::Gsw486), 61_000.0);
    assert_eq!(target("dhrystone", GswMode::Gsw586), 249_000.0);
    assert_eq!(target("whetstone", GswMode::Gsw486), 6.5);
    assert_eq!(target("whetstone", GswMode::Gsw586), 28.6);
    let p55c_dmips = target("dhrystone", GswMode::Gsw586) / VAX_DHRYSTONES_PER_SEC;
    assert!((p55c_dmips - 141.7).abs() < 0.5);
}

#[test]
fn cache_targets_descend_to_ram() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let l1 = target("bandwidth-l1", mode);
        let l2 = target("bandwidth-l2", mode);
        let ram = target("bandwidth-ram", mode);
        assert!(l1 > l2 && l2 > ram, "{mode:?}");
    }
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        assert!(
            target("bandwidth-l2", mode) > target("bandwidth-ram", mode),
            "{mode:?}"
        );
    }
}

#[test]
fn verdict_includes_both_band_edges() {
    let entry = band_for("dhrystone", GswMode::Gsw586).unwrap();
    assert_eq!(entry.verdict(entry.lo), BandVerdict::InBand);
    assert_eq!(entry.verdict(entry.hi), BandVerdict::InBand);
    assert_eq!(entry.verdict(entry.lo - 1.0), BandVerdict::Low);
    assert_eq!(entry.verdict(entry.hi + 1.0), BandVerdict::High);
}
