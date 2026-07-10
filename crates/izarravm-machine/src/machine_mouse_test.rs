// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn keyboard_out(machine: &mut Machine, port: u16, value: u8) {
    with_bus(machine, |bus| {
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    });
    while machine.read_io_port_u8(0x64) & 0x02 != 0 {
        let deadline = machine.keyboard.ticks_until_event().unwrap();
        machine.advance_devices_ticks(deadline);
    }
}

fn advance_8042_output(machine: &mut Machine) {
    while machine.read_io_port_u8(0x64) & 0x01 == 0 {
        let deadline = machine.keyboard.ticks_until_event().unwrap();
        machine.advance_devices_ticks(deadline);
    }
}

#[test]
fn mouse_movement_requests_irq12_after_enable() {
    // Bring up the PS/2 mouse the way a driver does (command byte bit 1 set
    // for the mouse interrupt, then 0xF4 enable reporting via the 0xD4 path),
    // then inject a host move and confirm IRQ12 is pending on the PIC and the
    // three-byte packet is readable on port 0x60 with the AUX status bit set.
    let profile = MachineProfile::gsw_386(1, VideoCard::Vega);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    // Drive the controller through the bus the way the CPU would, respecting
    // IBF and the auxiliary serial-byte deadline.
    keyboard_out(&mut machine, 0x64, 0x60); // write command byte
    keyboard_out(&mut machine, 0x60, 0x03); // IRQ1 + IRQ12 enabled
    keyboard_out(&mut machine, 0x64, 0xd4); // next byte to aux
    keyboard_out(&mut machine, 0x60, 0xf4); // enable data reporting
    advance_8042_output(&mut machine);
    assert_eq!(machine.read_io_port_u8(0x60), 0xfa); // mouse ACK
    // Move right 4, down 2, left button down.
    machine.inject_mouse(4, 2, 0x01);
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline);
    assert!(machine.irq12_pending(), "movement requests IRQ12");
    // The packet is on port 0x60 and the status reports an AUX byte.
    assert_eq!(machine.read_io_port_u8(0x64) & 0x20, 0x20, "AUX status bit");
    let b0 = machine.read_io_port_u8(0x60);
    assert_eq!(b0 & 0x08, 0x08, "always-one bit");
    assert_eq!(b0 & 0x01, 0x01, "left button");
    assert_eq!(b0 & 0x10, 0x00, "X positive");
    assert_eq!(b0 & 0x20, 0x20, "Y sign set (screen-down move)");
    advance_8042_output(&mut machine);
    assert_eq!(machine.read_io_port_u8(0x60), 4, "dx byte");
    advance_8042_output(&mut machine);
    assert_eq!(machine.read_io_port_u8(0x60) as i8 as i32, -2, "dy byte");
}

#[test]
fn bios_aux_enable_then_packet_reads_back_with_no_stray_keyboard_byte() {
    // Drive the exact sequence the BIOS bootbox menu runs (izbios-bootbox.inc
    // bx2_aux_init): read the controller command byte, set the IRQ1+IRQ12
    // enable bits, then enable AUX reporting via the 0xD4 prefix and drain the
    // mouse ACK. The two things this guards that the menu has no automated
    // coverage for: the injected packet reads back on 0x60 with the AUX status
    // bit set, AND the enable handshake never drops a stray byte into the
    // keyboard scancode ring (which the keyboard ISR reads unconditionally).
    let profile = MachineProfile::gsw_386(1, VideoCard::Vega);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    // Read CCB (0x20) -> 0x60, OR in IRQ1 (bit0) + IRQ12 (bit1), write back.
    keyboard_out(&mut machine, 0x64, 0x20);
    let ccb = machine.read_io_port_u8(0x60);
    let new_ccb = ccb | 0x01 | 0x02;
    keyboard_out(&mut machine, 0x64, 0x60);
    keyboard_out(&mut machine, 0x60, new_ccb);
    // Drain the IRQ1 edge the CCB read above itself arms in the controller
    // response path (a pre-existing quirk unrelated to AUX enable:
    // it fires for any controller-command response while command-byte
    // bit0 is set, which it is by default), then acknowledge it the way
    // the CPU eventually would so it doesn't linger as a pending PIC
    // request. This keeps the assertion below honestly testing whether
    // the AUX-enable sequence, not this earlier CCB read, arms IRQ1.
    machine.pic.acknowledge();
    // Enable AUX data reporting: 0xD4 routes 0xF4 to the mouse.
    keyboard_out(&mut machine, 0x64, 0xd4);
    keyboard_out(&mut machine, 0x60, 0xf4);
    advance_8042_output(&mut machine);
    // Drain the AUX ACK (0xFA): it must arrive flagged as an AUX byte.
    let status = machine.read_io_port_u8(0x64);
    assert_eq!(status & 0x01, 0x01, "ACK waiting (OBF)");
    assert_eq!(status & 0x20, 0x20, "ACK is an AUX byte, not a key");
    assert_eq!(machine.read_io_port_u8(0x60), 0xfa, "mouse ACK");
    // The AUX-enable sequence itself must not arm IRQ1.
    assert!(
        !machine.irq1_pending(),
        "AUX enable must not arm the keyboard interrupt"
    );
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x01,
        0,
        "no byte left in the output buffer after the ACK drain"
    );

    // Now a host move queues a three-byte packet, flagged AUX, with IRQ12.
    machine.inject_mouse(6, -3, 0x01); // right 6, up 3, left button down
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline);
    assert!(machine.irq12_pending(), "movement requests IRQ12");
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x20,
        0x20,
        "packet byte is flagged AUX"
    );
    let b0 = machine.read_io_port_u8(0x60);
    assert_eq!(b0 & 0x08, 0x08, "sync bit");
    assert_eq!(b0 & 0x01, 0x01, "left button");
    advance_8042_output(&mut machine);
    assert_eq!(machine.read_io_port_u8(0x60), 6, "dx byte");
    advance_8042_output(&mut machine);
    assert_eq!(
        machine.read_io_port_u8(0x60),
        3,
        "dy byte (screen up -> +3)"
    );
    // The packet drained cleanly: nothing left, and still no keyboard IRQ.
    assert_eq!(
        machine.read_io_port_u8(0x64) & 0x01,
        0,
        "output buffer empty after the packet"
    );
    assert!(
        !machine.irq1_pending(),
        "the AUX packet never touched the keyboard interrupt"
    );
}

#[test]
fn c200_enable_arms_irq12_in_the_command_byte_itself() {
    // Without any manual command-byte setup: a C200 enable must arm IRQ12 on
    // its own, the way a real PS/2 BIOS does, so the MOUSE.COM install path
    // (which only issues INT 15h C205/C207/C200) gets working interrupts. The
    // injected packet then raises IRQ12 with no separate command-byte write.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100); // BH=1 enable
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    let deadline = m.keyboard.ticks_until_event().unwrap();
    m.advance_devices_ticks(deadline);
    assert!(
        m.irq12_pending(),
        "C200 enable alone arms IRQ12 (no separate command-byte write needed)"
    );
}

#[test]
fn c205_initialize_arms_irq12_in_the_command_byte() {
    // C205 is MOUSE.COM's first BIOS call. Like C200 enable, it must arm IRQ12
    // on its own with no prior command-byte setup, so an injected packet raises
    // the interrupt.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC205);
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    let deadline = m.keyboard.ticks_until_event().unwrap();
    m.advance_devices_ticks(deadline);
    assert!(
        m.irq12_pending(),
        "C205 initialize alone arms IRQ12 (no separate command-byte write needed)"
    );
}

#[test]
fn c200_disable_leaves_no_irq12_pending() {
    // The BIOS-level mirror of the keyboard-level edge-clear test: enabling
    // then disabling the pointing device through C200 leaves a disabled mouse
    // that raises no IRQ12 (C200 disable both turns reporting off and clears
    // the command-byte IRQ12 bit). The keyboard unit test
    // disable_clears_a_pending_irq12_edge covers the already-latched-edge case.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0100); // BH=1 enable
    m.handle_int15();
    m.cpu.registers.set_eax(0xC200);
    m.cpu.registers.set_ebx(0x0000); // BH=0 disable
    m.handle_int15();
    m.inject_mouse(4, -2, 0x01);
    assert!(
        !m.irq12_pending(),
        "a disabled pointing device raises no IRQ12"
    );
}

#[test]
fn bios_irq12_preserves_interrupted_cx_dx() {
    // IRQ12 can interrupt any game code, even when the game never calls INT 33h.
    // The BIOS mouse ISR's dispatch helper uses CX/DX while assembling a packet,
    // so the outer ISR has to save them before IRET returns to the interrupted
    // instruction stream.
    const PROGRAM: &[u8] = &[
        0xb9, 0x34, 0x12, // mov cx,1234h
        0xba, 0x78, 0x56, // mov dx,5678h
        0xfb, // sti
        0xbb, 0xff, 0xff, // mov bx,ffffh
        0x4b, // dec bx
        0x75, 0xfd, // jnz $-3
        0x89, 0x0e, 0x00, 0x70, // mov [7000h],cx
        0x89, 0x16, 0x02, 0x70, // mov [7002h],dx
        0xfa, // cli
        0xf4, // hlt
    ];

    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        izarravm_firmware::izarra_bios(),
    )
    .unwrap();
    let _ = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    for (offset, byte) in PROGRAM.iter().copied().enumerate() {
        machine.write_physical_u8(0x8000 + offset as u32, byte);
    }

    machine.register_mouse_handler_for_test(0, 0); // null handler still exercises dispatch
    {
        let mut bus = machine.make_bus();
        bus.write_io(0x21, BusWidth::Byte, 0xfb, false).unwrap(); // master: IRQ2 only
        bus.write_io(0xa1, BusWidth::Byte, 0xef, false).unwrap(); // slave: IRQ12 only
    }

    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::real(0));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0));
    machine
        .cpu
        .registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::real(0));
    machine.cpu.registers.eip = 0x8000;
    machine.cpu.registers.eflags = 0x0002;
    machine.cpu.registers.set_esp(0x9000);

    machine.inject_mouse(7, 0, 0);
    let reason = machine.run_until_halt_or_cycles(10_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.read_physical_u16(0x7000), 0x1234, "CX survived");
    assert_eq!(machine.read_physical_u16(0x7002), 0x5678, "DX survived");
}

#[test]
fn set_mouse_absolute_synthesizes_relative_deltas() {
    let mut m = int15_machine(16);
    m.enable_8042_irq12();
    m.cpu.registers.set_eax(0xC205);
    m.handle_int15(); // initialize enables reporting
    m.seed_mouse_origin(100, 100);
    m.set_mouse_absolute(110, 97, 0x00); // +10 / -3 screen delta
    let deadline = m.keyboard.ticks_until_event().unwrap();
    m.advance_devices_ticks(deadline);
    assert!(
        m.irq12_pending(),
        "synthesized motion reaches the aux device"
    );
}

#[test]
fn bios_service_vectors_survive_low_memory_wipe() {
    // A booter that zeroes low RAM (including the 0x600 RAM IRET stub) must not
    // strand INT 11h/12h: their IVT targets point at the ROM IRET, so the
    // service still returns. Stub: zero 0x600, then INT 11h, then halt.
    // rom_with_code supplies the ROM IRET at FF00:0000 that survives the wipe.
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax, ax
        0x8E, 0xD8, // mov ds, ax
        0xC7, 0x06, 0x00, 0x06, 0x00, 0x00, // mov word [0x600], 0
        0xCD, 0x11, // int 11h
        0xF4, // hlt
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu().registers.eax() as u16, BIOS_EQUIPMENT_WORD);
}
