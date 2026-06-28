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
fn dhrystone_charges_fewer_clocks_in_586_than_386() {
    let i386 = run(GswMode::Gsw386).elapsed_clocks();
    let i586 = run(GswMode::Gsw586).elapsed_clocks();
    assert!(
        i586 < i386,
        "586 ({i586}) should charge fewer clocks than 386 ({i386})"
    );
}
