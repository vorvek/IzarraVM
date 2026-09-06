// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn neurketa_completion_preserves_guest_results_across_386_speeds() {
    let hardware = HardwareProfile::from_config(&AppConfig::default()).unwrap();
    let mut results = Vec::new();
    for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
        let baseline =
            run_bench_one(&hardware, mode, &BenchSource::BootSelector(0), 10_000_000).unwrap();
        assert_eq!((baseline.iterations, baseline.aux), (0, 0));
        let sieve =
            run_bench_one(&hardware, mode, &BenchSource::BootSelector(1), 100_000_000).unwrap();
        assert_eq!((sieve.iterations, sieve.aux), (40, 1899));
        results.push(sieve.clocks);
    }
    assert_ne!(results[0], results[1]);
}

#[test]
fn raw_exe_profiling_accepts_no_report_but_rejects_failed_exit() {
    for code in [0, 7] {
        let mut program = Vec::new();
        if code != 0 {
            program.extend_from_slice(&[
                0xb0, 25, 0xe6, 0xe4, 0xb0, 1, 0xe6, 0xe5, 0xb0, 17, 0xe6, 0xe4, 0xb0, 1, 0xe6,
                0xe5,
            ]);
        }
        program.extend_from_slice(&[
            0xb0, 12, 0xe6, 0xe4, 0xb0, code, 0xe6, 0xe5, 0xb0, 3, 0xe6, 0xe6, 0xf4,
        ]);
        let result = run_bench_one(
            &HardwareProfile::from_config(&AppConfig::default()).unwrap(),
            GswMode::Gsw586,
            &BenchSource::DosExe(program),
            100_000,
        );
        if code == 0 {
            let run = result.unwrap();
            assert_eq!(run.iterations, 0);
            assert!(
                bench_metrics(&run, GswMode::Gsw586)
                    .cycles_per_iter
                    .is_finite()
            );
            continue;
        }
        let error = result.err().expect("failed guest exits must be rejected");
        let text = error.to_string();
        assert!(text.contains("invalid completion"), "{text}");
        assert!(text.contains(&format!("code: {code}")), "{text}");
        assert!(text.contains("status=1, iterations=1"), "{text}");
    }
}

#[test]
fn an_empty_owned_payload_is_not_a_completed_benchmark() {
    let error = run_bench_one(
        &HardwareProfile::from_config(&AppConfig::default()).unwrap(),
        GswMode::Gsw586,
        &BenchSource::BootSelector(2),
        10_000_000,
    )
    .err()
    .expect("only the explicit baseline may report zero iterations");
    assert!(error.to_string().contains("status=1, iterations=0"));
}

#[test]
fn matching_a_historical_target_does_not_qualify_the_current_model() {
    let mode = GswMode::Gsw586;
    let reference = bench_reference::historical_band_for("dhrystone", mode).unwrap();
    assert_eq!(
        band_tag("dhrystone", mode, reference.target),
        " [unqualified historical ratio=1.00]"
    );
    let reference = bench_reference::historical_band_for("bandwidth-l2", mode).unwrap();
    assert_eq!(
        bandwidth_band_tag(mode, 64 * 1024, reference.target),
        "l2 [unqualified historical ratio=1.00]"
    );
}
