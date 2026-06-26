// Keyboard-layout integration test: the six Set-1 scancode -> ASCII tables the
// Izarra BIOS selects from CMOS byte 0x10.
//
// For each layout the test sets the CMOS layout byte, boots the BIOS to its idle
// loop (so the bring-up caches the layout into the BDA), injects one distinguishing
// scancode, then reads the BDA keyboard ring and asserts the translated ASCII. The
// INT 09h ISR stores each key as a word (scancode in the high byte, ASCII in the
// low byte) at the ring tail, so the low byte at the head is the ASCII the active
// layout produced.
//
// One distinguishing key per layout, chosen so each layout disagrees with en-US
// (and DE disagrees with IT, which share the QWERTZ Y/Z swap):
//   en-US sc 0x10 -> 'q'        UK sc 0x2b -> '#'         ES sc 0x27 -> n-tilde
//   FR    sc 0x10 -> 'a'        DE sc 0x0c -> eszett      IT sc 0x0c -> '\''
// FR and DE/IT also flip the QWERTY positions en-US keeps (0x10 'q', 0x15 'y').

use izarravm_core::VideoCard;
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile};

// CMOS NVRAM index of the persisted keyboard layout (MC146818 general area).
const CMOS_LAYOUT: usize = 0x10;

// BDA segment 0x40 -> physical 0x400. The keyboard ring head pointer is a word at
// offset 0x1a; the ring buffer starts at offset 0x1e.
const BDA_BASE: u32 = 0x0400;
const KB_BUF_HEAD: u32 = 0x001a;

// A handful of Set-1 make/break codes the distinguishing keys use.
const SC_Q_POS: u8 = 0x10; // physical Q key (US 'q', FR 'a')
const SC_MINUS: u8 = 0x0c; // physical key right of 0 (US '-', DE eszett, IT '\'')
const SC_SEMI: u8 = 0x27; // physical key right of L (US ';', ES n-tilde)
const SC_BACKSLASH: u8 = 0x2b; // ISO/ANSI key (US '\', UK '#')

fn break_of(make: u8) -> u8 {
    make | 0x80
}

/// Boot the BIOS with the given CMOS layout to its idle loop, inject one make/break
/// scancode, and return the ASCII the active layout translated it to from the BDA
/// ring. No setup hotkey is pressed, so POST runs and the BIOS parks at idle, where
/// nothing drains the ring before the read.
fn translate(layout: u8, scancode: u8) -> u8 {
    translate_sequence(layout, &[scancode, break_of(scancode)])
}

fn translate_sequence(layout: u8, scancodes: &[u8]) -> u8 {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarra_bios()).unwrap();
    // The bring-up caches CMOS 0x10 into the BDA, so set the layout before any run.
    machine.set_cmos_byte(CMOS_LAYOUT, layout);
    // Run past POST and the setup-hotkey window to the idle loop.
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    // Inject the key and let IRQ1 -> INT 09h translate and enqueue it.
    machine.inject_key_scancodes(scancodes);
    machine.run_until_halt_or_cycles(2_000_000).unwrap();
    // Read the ring head and the ASCII byte (low byte of the stored word) there.
    let head_lo = machine.read_physical_u8(BDA_BASE + KB_BUF_HEAD);
    let head_hi = machine.read_physical_u8(BDA_BASE + KB_BUF_HEAD + 1);
    let head = u32::from(head_lo) | (u32::from(head_hi) << 8);
    machine.read_physical_u8(BDA_BASE + head)
}

#[test]
fn en_us_default_is_qwerty() {
    // Layout 0, the default: the physical Q key is 'q'.
    assert_eq!(translate(0, SC_Q_POS), b'q');
}

#[test]
fn uk_extra_iso_key_is_hash() {
    // Layout 1: the ANSI backslash scancode carries '#' on the UK ISO board.
    assert_eq!(translate(1, SC_BACKSLASH), b'#');
    // The backslash is the US value, so this proves the table actually changed.
    assert_eq!(translate(0, SC_BACKSLASH), b'\\');
}

#[test]
fn es_semicolon_key_is_enye() {
    // Layout 2: the US semicolon scancode carries the Spanish n-tilde (CP437 0xA4).
    assert_eq!(translate(2, SC_SEMI), 0xa4);
    assert_eq!(translate(0, SC_SEMI), b';');
}

#[test]
fn es_shift_and_altgr_emit_symbols() {
    assert_eq!(translate_sequence(2, &[0x2a, 0x08, 0x88, 0xaa]), b'/');
    assert_eq!(
        translate_sequence(2, &[0xe0, 0x38, 0x03, 0x83, 0xe0, 0xb8]),
        b'@'
    );
}

#[test]
fn fr_azerty_swaps_q_to_a() {
    // Layout 3: AZERTY puts 'a' where US QWERTY has 'q'.
    assert_eq!(translate(3, SC_Q_POS), b'a');
    assert_eq!(translate(0, SC_Q_POS), b'q');
}

#[test]
fn de_qwertz_minus_key_is_eszett() {
    // Layout 4: the US minus scancode carries the German eszett (CP437 0xE1), and
    // QWERTZ puts 'z' where US QWERTY has 'y'.
    assert_eq!(translate(4, SC_MINUS), 0xe1);
    assert_eq!(translate(4, 0x15), b'z');
    assert_eq!(translate(0, 0x15), b'y');
}

#[test]
fn it_qwerty_minus_key_is_apostrophe() {
    // Layout 5: Italian is QWERTY (the MS-DOS source keeps Y/Z in the US
    // positions, unlike our earlier approximation that treated it as QWERTZ). It
    // puts an apostrophe on the US minus scancode, separating it from German.
    assert_eq!(translate(5, SC_MINUS), b'\'');
    assert_eq!(translate(5, 0x15), b'y');
    assert_eq!(
        translate_sequence(5, &[0xe0, 0x38, SC_SEMI, break_of(SC_SEMI), 0xe0, 0xb8]),
        b'@'
    );
}
