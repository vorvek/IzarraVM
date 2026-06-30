//! End-to-end checks that the Neurketa boot image runs in every CPU mode,
//! stops at the guest's CMD_EXIT, and reports the Sieve result primitives.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::neurketa_image;
use izarravm_machine::{Machine, MachineProfile, StopReason};

fn run(mode: GswMode, selector: u8) -> Machine {
    // Base profile mirrors the other machine tests; set_mode switches the mode.
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine =
        Machine::new_boot_image(profile, neurketa_image()).expect("neurketa image boots");
    machine.set_mode(mode);
    machine.set_bench_selector(selector);
    let stop = machine
        .run_until_halt_or_cycles(50_000_000_000)
        .expect("run does not fault");
    assert!(
        matches!(stop, StopReason::TestExit { code: 0 }),
        "expected a clean TestExit in {mode} mode, got {stop:?}"
    );
    machine
}

#[test]
fn sieve_reports_1899_primes_in_every_mode() {
    for mode in [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let machine = run(mode, 1);
        assert_eq!(machine.bench_status(), 1, "{mode} status");
        assert_eq!(machine.bench_iterations(), 40, "{mode} iterations");
        assert_eq!(machine.bench_aux(), 1899, "{mode} prime count");
    }
}

#[test]
fn fp_mandelbrot_is_consistent_across_the_fpu_modes() {
    // The FP payload (selector 3) runs only where there is an FPU (486 and 586).
    let m486 = run(GswMode::Gsw486, 3);
    let m586 = run(GswMode::Gsw586, 3);
    assert_eq!(m486.bench_status(), 1, "486 status");
    assert_eq!(m486.bench_iterations(), 1536, "pixel count");
    let csum = m486.bench_aux();
    assert_eq!(
        m586.bench_aux(),
        csum,
        "x87 is deterministic so 486 ({csum}) and 586 ({}) must agree",
        m586.bench_aux()
    );
    // Non-degenerate: not all pixels escaped on iteration 0-ish, and not all
    // pixels ran to maxiter. 1536 pixels, maxiter 64.
    assert_ne!(csum, 0, "checksum should be non-zero");
    assert!(
        csum > 1536,
        "more than one iteration per pixel on average somewhere"
    );
    assert!(csum < 1536 * 64, "not every pixel ran to maxiter");
}

#[test]
fn baseline_charges_fewer_clocks_than_the_sieve() {
    let baseline = run(GswMode::Gsw386, 0);
    let sieve = run(GswMode::Gsw386, 1);
    assert!(
        baseline.elapsed_clocks() < sieve.elapsed_clocks(),
        "baseline {} should be cheaper than sieve {}",
        baseline.elapsed_clocks(),
        sieve.elapsed_clocks()
    );
}

#[test]
fn fp_mandelbrot_runs_faster_in_586_than_486() {
    // Calibration note (B-T8/B-T9): 586's level_timing brakes fp-mandel into its
    // band and charges MORE instruction clocks per op than the 486 (whose ratio sits
    // near the bus floor), so 586's RAW fp-mandel clock total can exceed the 486's.
    // The meaningful metric is guest-perceived SPEED (pixels/sec): at 200 MHz vs
    // 66 MHz, 586 finishes far sooner. Assert that real speed ordering.
    fn pixels_per_sec(mode: GswMode) -> f64 {
        let m = run(mode, 3);
        let secs = m.elapsed_clocks() as f64 / mode.clock_hz() as f64;
        m.bench_iterations() as f64 / secs
    }
    let m486 = pixels_per_sec(GswMode::Gsw486);
    let m586 = pixels_per_sec(GswMode::Gsw586);
    assert!(
        m586 > m486,
        "586 ({m586:.0} px/s) should run faster than 486 ({m486:.0} px/s)"
    );
}

#[test]
fn faster_modes_run_the_sieve_faster() {
    // Calibration note (B-T8/B-T9): the Sieve is memory-bound, and the per-access bus
    // floor is mode-INDEPENDENT in clocks, so a faster mode does NOT necessarily
    // charge fewer raw clocks (its higher level_timing can add instruction clocks on
    // top of the same bus floor). The meaningful per-mode metric is guest-perceived
    // SPEED (passes/sec = iters / (clocks / clock_hz)): a faster mode's higher
    // clock_hz makes it finish sooner. Assert that real speed ordering.
    fn passes_per_sec(mode: GswMode) -> f64 {
        let m = run(mode, 1);
        let secs = m.elapsed_clocks() as f64 / mode.clock_hz() as f64;
        m.bench_iterations() as f64 / secs
    }
    let i386 = passes_per_sec(GswMode::Gsw386);
    let i486 = passes_per_sec(GswMode::Gsw486);
    let i586 = passes_per_sec(GswMode::Gsw586);
    assert!(
        i486 > i386,
        "486 ({i486:.1}/s) should be faster than 386 ({i386:.1}/s)"
    );
    assert!(
        i586 > i486,
        "586 ({i586:.1}/s) should be faster than 486 ({i486:.1}/s)"
    );
}
