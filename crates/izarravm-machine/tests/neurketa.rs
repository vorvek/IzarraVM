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
fn faster_modes_charge_fewer_guest_clocks_for_the_sieve() {
    let i386 = run(GswMode::Gsw386, 1).elapsed_clocks();
    let i486 = run(GswMode::Gsw486, 1).elapsed_clocks();
    let i586 = run(GswMode::Gsw586, 1).elapsed_clocks();
    assert!(
        i486 < i386,
        "486 ({i486}) should be cheaper than 386 ({i386})"
    );
    assert!(
        i586 < i486,
        "586 ({i586}) should be cheaper than 486 ({i486})"
    );
}
