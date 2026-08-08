// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[derive(Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct GswToml {
    cpu: GswMode,
}

#[test]
fn table_has_the_exact_four_profiles_in_rank_order() {
    let expected = [
        GswModeSpec {
            mode: GswMode::Gsw386Slow,
            canonical_name: "386-slow",
            rank: 0,
            register_code: 3,
            clock: ClockRate::new(22_000_000, 3),
            persona: CpuPersona::I386,
            cache: CacheGeometry {
                l1: L1Cache::None,
                external_kib: 64,
            },
        },
        GswModeSpec {
            mode: GswMode::Gsw386,
            canonical_name: "386",
            rank: 1,
            register_code: 0,
            clock: ClockRate::from_hz(22_000_000),
            persona: CpuPersona::I386,
            cache: CacheGeometry {
                l1: L1Cache::None,
                external_kib: 64,
            },
        },
        GswModeSpec {
            mode: GswMode::Gsw486,
            canonical_name: "486",
            rank: 2,
            register_code: 1,
            clock: ClockRate::from_hz(66_000_000),
            persona: CpuPersona::I486,
            cache: CacheGeometry {
                l1: L1Cache::Unified { kib: 8 },
                external_kib: 256,
            },
        },
        GswModeSpec {
            mode: GswMode::Gsw586,
            canonical_name: "586",
            rank: 3,
            register_code: 2,
            clock: ClockRate::from_hz(166_000_000),
            persona: CpuPersona::I586,
            cache: CacheGeometry {
                l1: L1Cache::Split {
                    instruction_kib: 16,
                    data_kib: 16,
                },
                external_kib: 512,
            },
        },
    ];

    assert_eq!(GSW_MODE_SPECS, expected);
    for (rank, spec) in GSW_MODE_SPECS.iter().enumerate() {
        assert_eq!(usize::from(spec.rank), rank);
        assert_eq!(spec.mode.spec(), spec);
    }
}

#[test]
fn names_codes_ranks_and_toml_round_trip_through_the_table() {
    for spec in GSW_MODE_SPECS {
        assert_eq!(GswMode::from_rank(spec.rank), Some(spec.mode));
        assert_eq!(
            GswMode::from_register_code(spec.register_code),
            Some(spec.mode)
        );
        assert_eq!(spec.canonical_name.parse::<GswMode>().unwrap(), spec.mode);

        let encoded = toml::to_string(&GswToml { cpu: spec.mode }).unwrap();
        assert_eq!(encoded, format!("cpu = \"{}\"\n", spec.canonical_name));
        assert_eq!(
            toml::from_str::<GswToml>(&encoded).unwrap(),
            GswToml { cpu: spec.mode }
        );
    }
    assert_eq!(GswMode::from_rank(4), None);
    assert_eq!(GswMode::from_register_code(4), None);
}

#[test]
fn retained_aliases_parse_without_changing_the_canonical_names() {
    for (alias, expected) in [
        ("slow", GswMode::Gsw386Slow),
        ("i386dx_25", GswMode::Gsw386),
        ("i486dx2_66", GswMode::Gsw486),
        ("gsw586", GswMode::Gsw586),
    ] {
        assert_eq!(alias.parse::<GswMode>().unwrap(), expected);
        assert_eq!(expected.to_string(), expected.canonical_name());
    }
    assert!("pentium_133".parse::<GswMode>().is_err());
}

#[test]
fn slow_386_is_exactly_one_third_by_cross_multiplication() {
    let slow = GswMode::Gsw386Slow.clock_rate();
    let normal = GswMode::Gsw386.clock_rate();
    let left = u128::from(slow.numerator_hz()) * 3 * u128::from(normal.denominator());
    let right = u128::from(normal.numerator_hz()) * u128::from(slow.denominator());

    assert_eq!(left, right);
    assert_eq!(slow.floor_hz(), 7_333_333);
    assert_eq!(
        GswMode::Gsw386Slow.cache_geometry(),
        GswMode::Gsw386.cache_geometry()
    );
    assert_eq!(GswMode::Gsw386Slow.persona(), GswMode::Gsw386.persona());
}

#[test]
fn removed_286_aliases_have_the_actionable_text_and_toml_error() {
    const MESSAGE: &str = "CPU preset '286' was removed; use '386-slow'";

    for alias in ["286", "80286", "i286", "gsw286"] {
        assert_eq!(alias.parse::<GswMode>().unwrap_err().to_string(), MESSAGE);
        let error = toml::from_str::<GswToml>(&format!("cpu = \"{alias}\"\n"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(MESSAGE), "{error}");
    }
}
