use izarravm_core::VideoCard;
use izarravm_firmware::izarra_bios;
use izarravm_machine::{Machine, MachineProfile};

const RESULT_STATUS: u32 = 0x8000; // packet status byte the handler saw
const RESULT_X: u32 = 0x8001; // packet X byte the handler saw
const RESULT_COUNT: u32 = 0x8002; // times the handler ran
const HANDLER_ADDR: u32 = 0x8100; // guest handler code at 0000:8100

// A far PS/2 mouse handler. Stack after `push bp; mov bp,sp`:
//   [bp+6]=Z, [bp+8]=Y, [bp+10]=X, [bp+12]=status.
// Saves status and X to RAM, bumps a counter, far-returns WITHOUT popping the four
// words (the BIOS ISR cleans them).
//   push bp; mov bp,sp; push ax; push ds; xor ax,ax; mov ds,ax
//   mov al,[bp+12]; mov [0x8000],al     ; status
//   mov al,[bp+10]; mov [0x8001],al     ; X
//   inc byte [0x8002]
//   pop ds; pop ax; pop bp; retf
const HANDLER: [u8; 31] = [
    0x55, 0x89, 0xe5, 0x50, 0x1e, 0x31, 0xc0, 0x8e, 0xd8, 0x8a, 0x46, 0x0c, 0xa2, 0x00, 0x80, 0x8a,
    0x46, 0x0a, 0xa2, 0x01, 0x80, 0xfe, 0x06, 0x02, 0x80, 0x1f, 0x58, 0x5d, 0xcb, 0x90, 0x90,
];

fn booted_to_idle() -> Machine {
    let profile = MachineProfile::gsw_386(16, VideoCard::Et4000Ax);
    let mut machine = Machine::new(profile, izarra_bios()).unwrap();
    machine.run_until_halt_or_cycles(20_000_000).unwrap();
    machine
}

#[test]
fn guest_int74_isr_calls_the_registered_handler() {
    let mut m = booted_to_idle();
    for (i, b) in HANDLER.iter().enumerate() {
        m.write_physical_u8(HANDLER_ADDR + i as u32, *b);
    }
    m.write_physical_u8(RESULT_STATUS, 0);
    m.write_physical_u8(RESULT_X, 0);
    m.write_physical_u8(RESULT_COUNT, 0);
    m.register_mouse_handler_for_test(0x0000, HANDLER_ADDR as u16);

    m.inject_mouse(7, -1, 0x01);
    m.run_until_halt_or_cycles(2_000_000).unwrap();

    assert_eq!(
        m.read_physical_u8(RESULT_COUNT),
        1,
        "handler ran once per packet"
    );
    assert_eq!(m.read_physical_u8(RESULT_X), 7, "X byte delivered");
    assert_eq!(
        m.read_physical_u8(RESULT_STATUS) & 0x01,
        0x01,
        "left button in status"
    );
}

#[test]
fn guest_int74_two_packets_call_handler_twice() {
    let mut m = booted_to_idle();
    for (i, b) in HANDLER.iter().enumerate() {
        m.write_physical_u8(HANDLER_ADDR + i as u32, *b);
    }
    m.write_physical_u8(RESULT_COUNT, 0);
    m.register_mouse_handler_for_test(0x0000, HANDLER_ADDR as u16);
    m.inject_mouse(3, 0, 0x00);
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    m.inject_mouse(-2, 0, 0x00);
    m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(
        m.read_physical_u8(RESULT_COUNT),
        2,
        "two packets, two calls"
    );
}
