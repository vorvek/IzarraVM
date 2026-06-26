// Shift make/break round-trips through the real run loop: 8042 -> IRQ1 ->
// INT 09h -> BDA KB_FLAGS. This pins where a "sticky shift" can and cannot come
// from. If these pass, a stuck shift is NOT the interrupt/8042/BIOS path; the
// break byte, once injected, reliably clears the flag. That points the blame at
// the host-input layer (the GUI deriving shift from egui's modifier state),
// which is where a release can fail to be turned into a 0xAA in the first place.

use izarravm_core::VideoCard;
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile};

// BDA 0x40:0x17 shift flags. Left shift is bit 1 (0x02), right shift bit 0.
const KB_FLAGS: u32 = 0x0417;
const LSHIFT: u8 = 0x02;

const SHIFT_MAKE: u8 = 0x2a;
const SHIFT_BREAK: u8 = 0xaa;

fn booted_to_idle() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarra_bios()).unwrap();
    // Past POST and the setup-hotkey window, to the idle loop with IRQ1 unmasked.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    machine
}

fn kb_flags(machine: &mut Machine) -> u8 {
    machine.read_physical_u8(KB_FLAGS)
}

/// The real shift case: make and break arrive in SEPARATE injects (the GUI sends
/// one batch per frame, and a held shift's press and release are different
/// frames). Each is a one-byte inject with its own IRQ1.
#[test]
fn separate_make_then_break_sets_then_clears_shift() {
    let mut machine = booted_to_idle();

    machine.inject_key_scancodes(&[SHIFT_MAKE]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        kb_flags(&mut machine) & LSHIFT,
        LSHIFT,
        "left shift held after make"
    );

    machine.inject_key_scancodes(&[SHIFT_BREAK]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        kb_flags(&mut machine) & LSHIFT,
        0,
        "left shift released after break"
    );
}

/// The multi-byte case: make and break in ONE inject. push_scancodes queues both
/// but only arms IRQ1 once; the break relies on advance_devices re-pulsing IRQ1
/// after the ISR reads 0x60. If the run loop drops that second edge, the flag
/// stays set and this fails.
#[test]
fn make_and_break_in_one_inject_still_clears_shift() {
    let mut machine = booted_to_idle();

    machine.inject_key_scancodes(&[SHIFT_MAKE, SHIFT_BREAK]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        kb_flags(&mut machine) & LSHIFT,
        0,
        "left shift released after a make+break in one batch"
    );
}

/// Hold shift across several other keystrokes, then release. Mirrors gameplay:
/// shift down, taps while held, shift up. The break must still clear the flag.
#[test]
fn shift_held_across_keys_then_released_clears() {
    let mut machine = booted_to_idle();

    machine.inject_key_scancodes(&[SHIFT_MAKE]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    // Tap the right-arrow position (0x4d) make/break a few times while shift is held.
    for _ in 0..3 {
        machine.inject_key_scancodes(&[0x4d, 0xcd]);
        machine.run_until_halt_or_cycles(1_000_000).unwrap();
    }
    assert_eq!(kb_flags(&mut machine) & LSHIFT, LSHIFT, "shift stays held");

    machine.inject_key_scancodes(&[SHIFT_BREAK]);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        kb_flags(&mut machine) & LSHIFT,
        0,
        "shift released at the end"
    );
}
