// End-to-end behavioural validation of the Toka-DOS MOUSE.COM driver.
//
// This is the first time the driver's INT 33h logic runs on the real CPU as a
// resident TSR. The test boots Toka-DOS on a temp C:, lets AUTOEXEC.BAT load
// MOUSE.COM and then run MTEST.COM (a guest self-test, source under
// tests/fixtures/mtest.asm), and asserts the self-test reports success through
// the Lotura unit-tester's Exit command.
//
// MTEST self-checks the driver in three stages and exits with a distinct code
// per failing check (0 = all passed):
//   A  reset (AX=0) returns AX=0xFFFF, BX=2                        -> fail 1
//   B  setpos/getpos round-trip                                   -> fail 2
//      range clamp                                                -> fail 3
//      AX=0x24 version (BX=0x0820, CX=0x0400)                     -> fail 4
//      reset re-centres to (319, 99)                              -> fail 5
//   C  host-injected motion seen via getpos                       -> fail 6 (none)
//      moved right + down                                         -> fail 7
//      left button set                                            -> fail 8
//
// The whole stack runs for real: boot -> DOS -> AUTOEXEC -> MOUSE TSR ->
// INT 15h C205/C207/C200 packet-handler registration -> INT 33h dispatcher, and
// for Stage C the host pointer -> 8042 -> IRQ12 -> BIOS INT 74h ISR ->
// MOUSE.COM packet handler -> INT 33h state.

use izarravm_core::{GswMode, VideoCard};
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile, StopReason};

// MTEST.COM, assembled from tests/fixtures/mtest.asm and committed alongside it.
// Regenerate after editing the asm: `nasm -f bin mtest.asm -o MTEST.COM` from the
// fixtures dir (same rebuild contract as the committed tokados.rom blob).
const MTEST_COM: &[u8] = include_bytes!("fixtures/MTEST.COM");
const CBLEAK_COM: &[u8] = include_bytes!("fixtures/CBLEAK.COM");
const GFXCUR_COM: &[u8] = include_bytes!("fixtures/GFXCUR.COM");
// WHEELTEST.COM, assembled from tests/fixtures/wheeltest.asm and committed
// alongside it (same rebuild contract as MTEST.COM). Exercises the SP-3b
// scroll-wheel path end-to-end: INT 33h fn 0x11 (wheel API) + fn 0x03 BH.
const WHEELTEST_COM: &[u8] = include_bytes!("fixtures/WHEELTEST.COM");

// AUTOEXEC.BAT: load the resident mouse driver, then run the self-test. MOUSE.COM
// installs to C:\DOS (on the PATH); MTEST.COM lands at the C: root, which is the
// boot drive's current directory, so a bare MTEST finds it.
const AUTOEXEC: &str = "@ECHO OFF\r\nC:\\DOS\\MOUSE\r\nMTEST\r\n";
const AUTOEXEC_CBLEAK: &str = "@ECHO OFF\r\nC:\\DOS\\MOUSE\r\nCBLEAK\r\n";
const AUTOEXEC_GFXCUR: &str = "@ECHO OFF\r\nC:\\DOS\\MOUSE\r\nGFXCUR\r\n";
const AUTOEXEC_WHEEL: &str = "@ECHO OFF\r\nC:\\DOS\\MOUSE\r\nWHEELTEST\r\n";

/// Install Toka-DOS onto a fresh temp C:, drop MTEST.COM at the root, write the
/// AUTOEXEC.BAT above, and return a booted machine plus the temp dir (kept alive
/// so the mount stays valid for the run).
fn boot_with_mtest() -> (Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let files = izarravm_firmware::toka_dos_system_files();
    izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format).unwrap();
    std::fs::write(dir.path().join("MTEST.COM"), MTEST_COM).unwrap();
    std::fs::write(dir.path().join("AUTOEXEC.BAT"), AUTOEXEC).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    // Pin to the 386 level this profile declares. The CPU resets to the full-ISA
    // I586 default, where the per-mode cycle seam scales clocks 2/5; that changes
    // how many guest instructions fit in each fixed CycleLimit chunk below, which
    // thins out the held-button motion stream and trips MTEST's Stage-C button
    // check. At I386 the seam is 1:1, restoring the cadence this self-test was
    // tuned against (the seam scales nothing on a 386).
    machine.set_mode(GswMode::Gsw386);
    machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
    machine.set_toka_c_root(dir.path().to_path_buf());
    (machine, dir)
}

fn boot_with_cbleak() -> (Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let files = izarravm_firmware::toka_dos_system_files();
    izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format).unwrap();
    std::fs::write(dir.path().join("CBLEAK.COM"), CBLEAK_COM).unwrap();
    std::fs::write(dir.path().join("AUTOEXEC.BAT"), AUTOEXEC_CBLEAK).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
    machine.set_toka_c_root(dir.path().to_path_buf());
    (machine, dir)
}

fn boot_with_gfxcur() -> (Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let files = izarravm_firmware::toka_dos_system_files();
    izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format).unwrap();
    std::fs::write(dir.path().join("GFXCUR.COM"), GFXCUR_COM).unwrap();
    std::fs::write(dir.path().join("AUTOEXEC.BAT"), AUTOEXEC_GFXCUR).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
    machine.set_toka_c_root(dir.path().to_path_buf());
    (machine, dir)
}

/// Install Toka-DOS onto a fresh temp C:, drop WHEELTEST.COM at the root, write
/// the wheel AUTOEXEC.BAT, and return a booted machine plus the temp dir.
fn boot_with_wheeltest() -> (Machine, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let files = izarravm_firmware::toka_dos_system_files();
    izarravm_dos::toka_dos_install(dir.path(), &files, izarravm_dos::InstallMode::Format).unwrap();
    std::fs::write(dir.path().join("WHEELTEST.COM"), WHEELTEST_COM).unwrap();
    std::fs::write(dir.path().join("AUTOEXEC.BAT"), AUTOEXEC_WHEEL).unwrap();

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    // Match the sibling mtest helper: pin the declared 386 level so the per-mode
    // cycle seam stays 1:1 and the chunked host injection keeps a steady cadence.
    machine.set_mode(GswMode::Gsw386);
    machine.mount_c_drive(izarravm_dos::HostDrive::mount_c(dir.path()).unwrap());
    machine.set_toka_c_root(dir.path().to_path_buf());
    (machine, dir)
}

fn type_line(machine: &mut Machine, text: &str) {
    for ch in text.chars().chain(std::iter::once('\r')) {
        let codes: &[u8] = match ch {
            'v' | 'V' => &[0x2f, 0xaf],
            'e' | 'E' => &[0x12, 0x92],
            'r' | 'R' => &[0x13, 0x93],
            '\r' => &[0x1c, 0x9c],
            _ => &[],
        };
        assert!(!codes.is_empty(), "unmapped test character {ch:?}");
        for code in codes {
            machine.inject_key_scancodes(&[*code]);
        }
        machine.run_until_halt_or_cycles(500_000).unwrap();
    }
    machine.run_until_halt_or_cycles(8_000_000).unwrap();
}

#[test]
fn mouse_driver_passes_the_guest_self_test_end_to_end() {
    let (mut machine, _dir) = boot_with_mtest();

    // Drive the run in chunks. MTEST's Stage A/B run uninterrupted; if a static
    // check fails it exits early and the chunk returns TestExit with the failing
    // code. Stage C then spins in a bounded getpos poll waiting for host motion,
    // so the chunk returns CycleLimit. The first chunks cover POST + disk boot +
    // DOS + MOUSE install, so a CycleLimit can mean either "still booting" or
    // "now polling" - we cannot tell from outside. So inject on every CycleLimit
    // until MTEST exits. A move that lands before the poll (during boot or a
    // static check) is harmless: the poll re-centres its baseline just before it
    // starts, and a later move lands inside the poll. Injecting each chunk holds
    // the left button down and keeps moving right+down, exactly what the poll
    // checks, so whichever chunk the poll is live in, a move is waiting. This
    // makes the test robust to where the chunk boundaries fall.
    // The host injects the held-button motion once per chunk, so the chunk must be
    // small enough that the held left button streams as a dense run of packets (the
    // way a real held button does), not a sparse pulse. With a coarse chunk, MTEST's
    // Stage-C poll can latch onto a button-less driver-init event in the gap before
    // the next injection and report a (correct) motion with no button held - a
    // false failure that depends on exactly where the boot cycle count lands. A fine
    // cadence keeps the button continuously asserted across the poll. The total
    // budget (MAX_CHUNKS * CHUNK) still covers boot + DOS + MOUSE install + the test.
    const CHUNK: u64 = 2_000_000;
    const MAX_CHUNKS: usize = 120;
    let mut final_reason = None;
    for _ in 0..MAX_CHUNKS {
        match machine.run_until_halt_or_cycles(CHUNK).unwrap() {
            StopReason::TestExit { code } => {
                final_reason = Some(StopReason::TestExit { code });
                break;
            }
            // Equal mickeys right + down, left button held. The packet handler
            // scales this through the mickey-to-pixel ratio (8 horizontal, 16
            // vertical) and adds the pixel delta to cur_x/cur_y, so MTEST sees the
            // cursor move twice as far in x as in y. Small per-chunk steps keep the
            // cursor off the range edges no matter how many chunks land.
            StopReason::CycleLimit { .. } => machine.inject_mouse(8, 8, 0x01),
            other => {
                final_reason = Some(other);
                break;
            }
        }
    }

    match final_reason {
        Some(StopReason::TestExit { code: 0 }) => {}
        Some(StopReason::TestExit { code }) => panic!(
            "MTEST reported failure code {code} (1=reset status, 2=setpos/getpos, \
             3=range clamp, 4=version 0x24, 5=reset re-centre, 6=no motion seen, \
             7=motion direction, 8=left button, 9=ratio not applied)"
        ),
        Some(other) => panic!(
            "expected MTEST to Exit with code 0, got {other:?}; the self-test never \
             reached its Lotura Exit (check that MOUSE.COM went resident and MTEST ran)"
        ),
        None => panic!(
            "MTEST never exited within {MAX_CHUNKS} chunks; it may be stuck before \
             the poll, or the injected motion never reached the driver"
        ),
    }
}

#[test]
fn mouse_wheel_reported_over_int33_end_to_end() {
    let (mut machine, _dir) = boot_with_wheeltest();

    // Drive the run in chunks. WHEELTEST's Stage A (the wheel-API probe) runs
    // uninterrupted; if it fails it exits early and the chunk returns TestExit
    // with code 1. Stage B then spins in a bounded getpos poll waiting for a
    // host-injected wheel detent, so the chunk returns CycleLimit. The early
    // chunks cover POST + disk boot + DOS + MOUSE install, so a CycleLimit can
    // mean either "still booting" or "now polling" - we cannot tell from
    // outside, so inject a positive wheel detent on every CycleLimit until
    // WHEELTEST exits. A detent that lands before the poll starts is harmless:
    // fn 0x03 consumes the counter each call, so a later detent lands inside the
    // poll. inject_mouse_wheel(1) makes the device emit Z=+1, the BIOS INT 74h
    // ISR sign-extends it, the driver accumulates a positive [wheel], and fn
    // 0x03 returns BH > 0 (non-zero, top bit clear) - exactly what Stage B
    // checks for a positive injection.
    const CHUNK: u64 = 2_000_000;
    const MAX_CHUNKS: usize = 120;
    let mut final_reason = None;
    for _ in 0..MAX_CHUNKS {
        match machine.run_until_halt_or_cycles(CHUNK).unwrap() {
            StopReason::TestExit { code } => {
                final_reason = Some(StopReason::TestExit { code });
                break;
            }
            // Positive wheel detent (no motion, buttons unchanged). Only queues a
            // 4-byte packet once the mouse is in IntelliMouse mode, which the
            // BIOS C200/C205 enable (driven by the resident MOUSE TSR) turns on.
            StopReason::CycleLimit { .. } => machine.inject_mouse_wheel(1),
            other => {
                final_reason = Some(other);
                break;
            }
        }
    }

    match final_reason {
        Some(StopReason::TestExit { code: 0 }) => {}
        Some(StopReason::TestExit { code }) => panic!(
            "WHEELTEST reported failure code {code} (1=fn 0x11 wheel API absent, \
             2=wheel sign wrong (BH negative for a positive injection), \
             3=no wheel movement seen in fn 0x03 BH)"
        ),
        Some(other) => panic!(
            "expected WHEELTEST to Exit with code 0, got {other:?}; the self-test \
             never reached its Lotura Exit (check that MOUSE.COM went resident and \
             WHEELTEST ran)"
        ),
        None => panic!(
            "WHEELTEST never exited within {MAX_CHUNKS} chunks; it may be stuck \
             before the poll, or the injected wheel detent never reached the driver"
        ),
    }
}

#[test]
fn mouse_driver_does_not_call_a_child_callback_after_exec_return() {
    let (mut machine, _dir) = boot_with_cbleak();
    let reason = machine.run_until_halt_or_cycles(80_000_000).unwrap();
    assert!(
        !matches!(reason, StopReason::TestExit { code: 99 }),
        "CBLEAK's callback fired before the child returned"
    );
    let text = machine.screen_text().as_text();
    assert!(
        text.contains("C:\\>"),
        "CBLEAK should have returned to the shell prompt; got:\n{text}"
    );
    machine.inject_mouse(8, 0, 0);
    let reason = machine.run_until_halt_or_cycles(4_000_000).unwrap();
    assert!(
        !matches!(reason, StopReason::TestExit { code: 99 }),
        "stale child mouse callback fired after EXEC return"
    );

    type_line(&mut machine, "ver");
    let text = machine.screen_text().as_text();
    assert!(
        text.contains("Toka-DOS v3.0"),
        "shell should still accept commands after post-child mouse motion; got:\n{text}"
    );
}

#[test]
fn mouse_driver_does_not_draw_text_cursor_in_graphics_modes() {
    let (mut machine, _dir) = boot_with_gfxcur();
    let reason = machine.run_until_halt_or_cycles(80_000_000).unwrap();
    match reason {
        StopReason::TestExit { code: 0 } => {}
        StopReason::TestExit { code } => panic!(
            "GFXCUR reported failure code {code} (1=mouse reset failed, \
             2=graphics-mode Show Cursor wrote through B800)"
        ),
        other => panic!("expected GFXCUR to exit through the unit tester, got {other:?}"),
    }
}
