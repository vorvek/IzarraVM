//! Dhrystone runs as a freestanding DOS .EXE and produces the same correct
//! self-check result in every CPU mode (the CPU computes mode-independently).
use izarravm_core::{GswMode, VideoCard};
use izarravm_machine::{Machine, MachineProfile, StopReason};

fn run(mode: GswMode) -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new_dos_program(profile, izarravm_firmware::dhrystone_exe())
        .expect("load dhrystone");
    machine.set_mode(mode);
    let stop = machine
        .run_until_halt_or_cycles(50_000_000_000)
        .expect("run does not fault");
    assert!(
        matches!(stop, StopReason::TestExit { code: 0 }),
        "dhrystone did not complete cleanly in {mode} mode: {stop:?}"
    );
    machine
}

#[test]
fn dhrystone_self_check_is_correct_in_every_mode() {
    for mode in [
        GswMode::Gsw286,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        let machine = run(mode);
        assert_eq!(machine.bench_iterations(), 10000, "{mode} iterations");
        // 10214 is the deterministic Dhrystone self-check fold for Number_Of_Runs=10000.
        assert_eq!(machine.bench_aux(), 10214, "{mode} self-check checksum");
    }
}

#[test]
fn dhrystone_runs_faster_in_586_than_386() {
    // Calibration note (B-T8/B-T9): the meaningful per-mode metric is guest-perceived
    // SPEED (Dhrystones/sec = iters / (clocks / clock_hz)), not the raw clock count.
    // 586's level_timing (1/3) brakes fp-mandel into its band and so charges MORE
    // instruction clocks per op than 386, which can make 586's RAW Dhrystone clock
    // total exceed 386's. But 586 runs at 266 MHz vs 386's 22 MHz, so its
    // Dhrystones/sec is far higher. Assert that real speed ordering, not the clocks.
    fn dhrystones_per_sec(mode: GswMode) -> f64 {
        let m = run(mode);
        let secs = m.elapsed_clocks() as f64 / mode.clock_hz() as f64;
        m.bench_iterations() as f64 / secs
    }
    let i386 = dhrystones_per_sec(GswMode::Gsw386);
    let i586 = dhrystones_per_sec(GswMode::Gsw586);
    assert!(
        i586 > i386,
        "586 ({i586:.0} D/s) should run faster than 386 ({i386:.0} D/s)"
    );
}
