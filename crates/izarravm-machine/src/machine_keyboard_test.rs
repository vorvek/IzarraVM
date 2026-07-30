// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn injected_key_is_readable_on_port_0x60_and_requests_irq1() {
    // A bare machine: inject a scancode, then read it back through the bus the
    // way the CPU would, and confirm IRQ1 became pending on the PIC.
    let profile = MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega);
    let mut machine = Machine::new(profile, vec![0u8; BIOS_ROM_SIZE]).unwrap();
    machine.inject_key_scancodes(&[0x1e]); // 'A' make
    let deadline = machine.keyboard.ticks_until_event().unwrap();
    machine.advance_devices_ticks(deadline);
    assert_eq!(machine.read_io_port_u8(0x60), 0x1e);
    assert!(machine.irq1_pending(), "injecting a key requests IRQ1");
}

#[test]
fn int16_peek_empty_sets_guest_zf() {
    // mov al,1; or al,al; mov ah,1; int 16h; jz pass; mov al,1; jmp exit;
    // pass: xor al,al; exit: store AL as the Lotura exit code.
    const PROG: [u8; 30] = [
        0xb0, 0x01, 0x08, 0xc0, 0xb4, 0x01, 0xcd, 0x16, 0x74, 0x04, 0xb0, 0x01, 0xeb, 0x02, 0x30,
        0xc0, 0x88, 0xc3, 0xb0, 0x0c, 0xe6, 0xe4, 0x88, 0xd8, 0xe6, 0xe5, 0xb0, 0x03, 0xe6, 0xe6,
    ];
    assert_eq!(
        int16_peek_guest_exit(&[], &PROG),
        StopReason::TestExit { code: 0 }
    );
}

#[test]
fn int16_peek_available_clears_guest_zf() {
    // xor ax,ax; mov ah,1; int 16h; jnz pass; mov al,1; jmp exit;
    // pass: xor al,al; exit: store AL as the Lotura exit code.
    const PROG: [u8; 28] = [
        0x31, 0xc0, 0xb4, 0x01, 0xcd, 0x16, 0x75, 0x04, 0xb0, 0x01, 0xeb, 0x02, 0x30, 0xc0, 0x88,
        0xc3, 0xb0, 0x0c, 0xe6, 0xe4, 0x88, 0xd8, 0xe6, 0xe5, 0xb0, 0x03, 0xe6, 0xe6,
    ];
    assert_eq!(
        int16_peek_guest_exit(&[0x1e, 0x9e], &PROG),
        StopReason::TestExit { code: 0 }
    );
}

#[test]
fn int16_returns_extended_scancode_for_up_arrow() {
    // Up arrow is the bare scancode 0x48 (make) / 0xC8 (break); no 0xE0 prefix.
    // The layout table has no ASCII for it, so INT 16h returns scancode 0x48
    // with ASCII 0 -- the value a full-screen editor keys arrow navigation off.
    assert_eq!(int16_read_after(&[0x48, 0xC8]), 0x4800);
}

#[test]
fn int16_emits_control_code_for_ctrl_s() {
    // Ctrl down, S, S up, Ctrl up. Holding Ctrl turns S into the DC3 control
    // code (0x13), the way a real BIOS does, so the editor reads Ctrl-S as a
    // single ring entry (scancode 0x1f, ASCII 0x13) with no modifier polling.
    assert_eq!(int16_read_after(&[0x1d, 0x1f, 0x9f, 0x9d]), 0x1f13);
}

#[test]
fn int16_numpad_honors_num_lock_for_digits() {
    // NumLock make/break toggles the BIOS flag without entering the key ring.
    // The next keypad make then returns its numeric ASCII byte.
    assert_eq!(int16_read_after(&[0x45, 0xc5, 0x48, 0xc8]), 0x4838);
    assert_eq!(int16_read_after(&[0x45, 0xc5, 0x52, 0xd2]), 0x5230);
    assert_eq!(int16_read_after(&[0x4e, 0xce]), 0x4e2b);
}

#[test]
fn int16_resident_keyboard_uses_bios_layout_byte() {
    assert_eq!(int16_read_after_with_layout(0, &[0x27, 0xa7]), 0x273b);
    assert_eq!(
        int16_read_after_with_layout(0, &[0x2a, 0x27, 0xa7, 0xaa]),
        0x273a
    );
    assert_eq!(int16_read_after_with_layout(2, &[0x27, 0xa7]), 0x27a4);
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x33, 0xb3, 0xaa]),
        0x333b
    );
}

#[test]
fn es_layout_fills_ordinal_and_iso_and_cedilla() {
    // AX = (scancode << 8) | ascii from INT 16h. Layout 2 = ES.
    // 0x29 (left of 1): º / ª / backslash
    assert_eq!(int16_read_after_with_layout(2, &[0x29, 0xa9]), 0x29a7); // º CP437 0xA7
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x29, 0xa9, 0xaa]),
        0x29a6
    ); // ª 0xA6
    assert_eq!(
        int16_read_after_with_layout(2, &[0xe0, 0x38, 0x29, 0xa9, 0xe0, 0xb8]),
        0x295c
    ); // AltGr+0x29 -> '\'
    // 0x56 ISO 102nd key: < / >
    assert_eq!(int16_read_after_with_layout(2, &[0x56, 0xd6]), 0x563c); // '<'
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x56, 0xd6, 0xaa]),
        0x563e
    ); // '>'
    // 0x2b cedilla key: c-cedilla / C-cedilla (was wrongly Greek glyphs)
    assert_eq!(int16_read_after_with_layout(2, &[0x2b, 0xab]), 0x2b87); // ç 0x87
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x2b, 0xab, 0xaa]),
        0x2b80
    ); // Ç 0x80
    // 0x0d inverted punctuation: ¡ / ¿
    assert_eq!(int16_read_after_with_layout(2, &[0x0d, 0x8d]), 0x0dad); // ¡ 0xAD
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x0d, 0x8d, 0xaa]),
        0x0da8
    ); // ¿ 0xA8
}

#[test]
fn es_dead_keys_compose_accents() {
    // Layout 2 = ES. 0x28 = acute dead key, 0x1a = grave dead key.
    // acute + a -> a-acute (CP437 0xA0), reported with a's scancode 0x1e.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x1e, 0x9e]),
        0x1ea0
    );
    // grave + e -> e-grave (0x8A), e scancode 0x12.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x1a, 0x9a, 0x12, 0x92]),
        0x128a
    );
    // diaeresis (shift+0x28) + u -> u-diaeresis (0x81), u scancode 0x16.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x28, 0xa8, 0xaa, 0x16, 0x96]),
        0x1681
    );
    // acute + space -> the spacing acute ' (0x27), space scancode 0x39.
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x39, 0xb9]),
        0x3927
    );
    // acute + t (no composed form) -> first key read back is the flush ' (0x27).
    assert_eq!(
        int16_read_after_with_layout(2, &[0x28, 0xa8, 0x14, 0x94]),
        0x1427
    );
}

#[test]
fn non_es_dead_keys_use_the_per_layout_tables() {
    // The kb_deadkey routine now selects the descriptor and composition table
    // by KB_LAYOUT, so dead keys work beyond Spanish. French (layout 3): the
    // circumflex dead key (scancode 0x1a, unshifted) then 'e' (0x12) composes
    // to e-circumflex (CP850 0x88), reported with e's scancode.
    assert_eq!(
        int16_read_after_with_layout(3, &[0x1a, 0x9a, 0x12, 0x92]),
        0x1288
    );
    // German (layout 4): the dead acute (scancode 0x0d, unshifted) then 'a'
    // (0x1e) composes to a-acute (CP850 0xA0).
    assert_eq!(
        int16_read_after_with_layout(4, &[0x0d, 0x8d, 0x1e, 0x9e]),
        0x1ea0
    );
}

#[test]
fn int16_enhanced_read_matches_plain_read() {
    // AH=10h must hand a DOS program the same ring entry AH=00h does. Up
    // arrow gives scancode 0x48 / ASCII 0, the editor-navigation case.
    assert_eq!(int16_enhanced_read_after(&[0x48, 0xC8]), 0x4800);
    assert_eq!(
        int16_enhanced_read_after(&[0x48, 0xC8]),
        int16_read_after(&[0x48, 0xC8]),
    );
}

#[test]
fn int16_92_advertises_enhanced_keyboard_services() {
    // mov ax,9217h; int 16h; mov [0200h],ax; int 20h
    const PROG: [u8; 10] = [0xb8, 0x17, 0x92, 0xcd, 0x16, 0xa3, 0x00, 0x02, 0xcd, 0x20];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(
        read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200),
        0x8017
    );
}

#[test]
fn int16_keyclick_call_returns_to_caller() {
    // mov ax,0401h; int 16h; mov word [0200h],1234h; int 20h
    const PROG: [u8; 13] = [
        0xb8, 0x01, 0x04, 0xcd, 0x16, 0xc7, 0x06, 0x00, 0x02, 0x34, 0x12, 0xcd, 0x20,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
    machine.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(
        read_u16(&mut machine, (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x200),
        0x1234,
        "INT 16h AH=04h returned to the caller"
    );
}

#[test]
fn io_port_reports_last_post_write() {
    // mov al,0x42; out 0x80,al; hlt
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Vega),
        rom_with_code(&[0xb0, 0x42, 0xe6, 0x80, 0xf4]),
    )
    .unwrap();
    machine.run_until_halt_or_cycles(10_000).unwrap();
    assert_eq!(machine.io_port(0x80), Some(0x42));
    assert_eq!(machine.io_port(0x0100), None); // outside the passive port map
}
