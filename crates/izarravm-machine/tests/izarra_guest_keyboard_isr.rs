// Reproduces how an action game (Prince of Persia) reads the keyboard: it
// installs its OWN INT 09h handler, reads raw scancodes from port 0x60, and
// tracks key state from the make/break codes (a key release is scancode | 0x80).
// The BIOS ring (INT 16h) is useless for that because releases never enqueue, so
// PoP must hook INT 09h. This test installs a minimal guest handler over the
// booted BIOS, points IVT[9] at it, injects a make then a break, and checks the
// guest handler saw BOTH. If the break never reaches a guest-revectored handler,
// that is the "can't stop walking / sticky shift" bug; if it does, custom-ISR
// keyboard delivery is sound and the fault is elsewhere.

use izarravm_core::VideoCard;
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile};

// A free low-RAM scratch area after a bare BIOS boot (above the BDA, below the
// boot-sector load address): results and the handler code.
const RESULT_LAST: u32 = 0x8000; // last scancode the guest ISR read from 0x60
const RESULT_COUNT: u32 = 0x8001; // how many times the guest ISR ran
const HANDLER_ADDR: u32 = 0x8100; // guest ISR code, segment 0 offset 0x8100
const IVT9: u32 = 0x09 * 4; // IVT entry for INT 09h (offset at 0x24, seg at 0x26)

// A minimal real-mode INT 09h: save regs, read the scancode, store it and bump a
// counter at 0000:RESULT_*, send the PIC end-of-interrupt, restore, iret.
//   push ax; push ds; xor ax,ax; mov ds,ax
//   in al,0x60; mov [0x8000],al; inc byte [0x8001]
//   mov al,0x20; out 0x20,al
//   pop ds; pop ax; iret
const HANDLER: [u8; 22] = [
    0x50, 0x1e, 0x31, 0xc0, 0x8e, 0xd8, 0xe4, 0x60, 0xa2, 0x00, 0x80, 0xfe, 0x06, 0x01, 0x80, 0xb0,
    0x20, 0xe6, 0x20, 0x1f, 0x58, 0xcf,
];

fn booted_to_idle() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    machine
}

fn install_guest_isr(m: &mut Machine) {
    for (i, b) in HANDLER.iter().enumerate() {
        m.write_physical_u8(HANDLER_ADDR + i as u32, *b);
    }
    m.write_physical_u8(RESULT_LAST, 0);
    m.write_physical_u8(RESULT_COUNT, 0);
    // Point IVT[9] at the guest handler at 0000:8100.
    m.write_physical_u16(IVT9, HANDLER_ADDR as u16);
    m.write_physical_u16(IVT9 + 2, 0x0000);
}

// Prince of Persia's actual INT 09h handler logic (disassembled from a running
// guest at 1530:00d6), reproduced standalone: read the scancode, derive
// "pressed = 1 / released = 0" via rol+and+xor, and store it into a per-scancode
// key_states table. If our CPU executes this sequence correctly, a break writes
// 0 and the prince stops; if not, that is the "can't stop walking" bug.
//   push ds; push ax; push bx; xor ax,ax; mov ds,ax
//   in al,0x60; mov bl,al; rol al,1; and al,1; xor al,1   ; al = make?1:0
//   xor bh,bh; and bl,0x7f; add bx,0x9000; mov [bx],al     ; key_states[sc] = al
//   mov al,0x20; out 0x20,al; pop bx; pop ax; pop ds; iret
const POP_HANDLER: [u8; 36] = [
    0x1e, 0x50, 0x53, 0x31, 0xc0, 0x8e, 0xd8, 0xe4, 0x60, 0x8a, 0xd8, 0xd0, 0xc0, 0x24, 0x01, 0x34,
    0x01, 0x30, 0xff, 0x80, 0xe3, 0x7f, 0x81, 0xc3, 0x00, 0x90, 0x88, 0x07, 0xb0, 0x20, 0xe6, 0x20,
    0x5b, 0x58, 0x1f, 0xcf,
];
const KEY_STATES: u32 = 0x9000; // key_states[scancode], DS=0

/// Reproduces PoP's own key-state logic on the real CPU: make sets the slot to 1,
/// break must clear it to 0. If the break leaves it at 1, the prince keeps
/// walking. This isolates whether the fault is in executing PoP's rol/and/xor +
/// store sequence (a CPU bug) versus elsewhere.
#[test]
fn pop_style_handler_clears_key_state_on_break() {
    let mut m = booted_to_idle();
    for (i, b) in POP_HANDLER.iter().enumerate() {
        m.write_physical_u8(HANDLER_ADDR + i as u32, *b);
    }
    m.write_physical_u8(KEY_STATES + 0x4b, 0xff); // numpad-4 slot, poisoned
    m.write_physical_u16(IVT9, HANDLER_ADDR as u16);
    m.write_physical_u16(IVT9 + 2, 0x0000);

    m.inject_key_scancodes(&[0x4b]); // make
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        m.read_physical_u8(KEY_STATES + 0x4b),
        1,
        "make should set key_states[0x4b] = 1"
    );

    m.inject_key_scancodes(&[0xcb]); // break
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        m.read_physical_u8(KEY_STATES + 0x4b),
        0,
        "break must clear key_states[0x4b] = 0 (else the prince never stops)"
    );
}

/// A guest-revectored INT 09h must receive the make AND the break, the same way
/// Prince of Persia's own handler does.
#[test]
fn guest_int09_receives_make_then_break() {
    let mut m = booted_to_idle();
    install_guest_isr(&mut m);

    m.inject_key_scancodes(&[0x4b]); // numpad-4 make
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        m.read_physical_u8(RESULT_LAST),
        0x4b,
        "guest ISR should read the make code"
    );

    m.inject_key_scancodes(&[0xcb]); // numpad-4 break
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        m.read_physical_u8(RESULT_LAST),
        0xcb,
        "guest ISR should read the break code (the release PoP needs to stop walking)"
    );
    assert_eq!(
        m.read_physical_u8(RESULT_COUNT),
        2,
        "guest ISR should have run once per scancode"
    );
}
