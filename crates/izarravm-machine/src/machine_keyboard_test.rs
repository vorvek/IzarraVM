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
    // with ASCII 0, the value a full-screen editor keys arrow navigation off.
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

#[test]
fn int16_ignores_the_international_keys_and_keeps_the_ring_sane() {
    // IntlRo (73h), IntlYen (7Dh) and the keypad comma (7Eh) have no entry in
    // the layout tables. Make and break of each must leave the ring empty, so
    // the next real key is the first thing INT 16h returns.
    assert_eq!(
        int16_read_after(&[0x73, 0xf3, 0x7d, 0xfd, 0x7e, 0xfe, 0x1e, 0x9e]),
        0x1e61
    );
}

#[test]
fn a_normal_key_after_an_ignored_code_still_arrives() {
    assert_eq!(int16_read_after(&[0x73, 0x1e, 0x9e]), 0x1e61);
    assert_eq!(int16_read_after(&[0x7d, 0xfd, 0x7e, 0x1e]), 0x1e61);
}

#[test]
fn e0_prefixed_ignored_code_leaves_the_prefix_clean() {
    // E0 5Bh is above the table. It enqueues nothing and clears the prefix, so
    // the plain Home key after it carries no gray-key marker in AL.
    assert_eq!(
        int16_enhanced_read_after(&[0xe0, 0x5b, 0xe0, 0xdb, 0x47, 0xc7]),
        0x4700
    );
}

/// Run a raw guest program that ends in a delay and a Lotura test exit with
/// code 0. The delay lets the 8042 finish any command before the exit.
fn run_reset_probe(body: &[u8]) -> (Machine, StopReason) {
    // mov cx,0xFFFF; loop $; xor al,al; then Lotura: index 12, data 0, exit.
    const TAIL: [u8; 17] = [
        0xB9, 0xFF, 0xFF, 0xE2, 0xFE, 0xB0, 0x0C, 0xE6, 0xE4, 0x30, 0xC0, 0xE6, 0xE5, 0xB0, 0x03,
        0xE6, 0xE6,
    ];
    let mut prog = body.to_vec();
    prog.extend_from_slice(&TAIL);
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
    let stop = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    (machine, stop)
}

#[test]
fn guest_out_64h_fe_requests_a_reset() {
    // mov al,0xFE; out 0x64,al
    let (_, stop) = run_reset_probe(&[0xB0, 0xFE, 0xE6, 0x64]);
    assert_eq!(stop, StopReason::ResetRequested);
}

#[test]
fn guest_d1_then_fe_requests_a_reset() {
    // mov al,0xD1; out 0x64,al; wait for IBF clear; mov al,0xFE; out 0x60,al
    let (_, stop) = run_reset_probe(&[
        0xB0, 0xD1, 0xE6, 0x64, 0xE4, 0x64, 0xA8, 0x02, 0x75, 0xFA, 0xB0, 0xFE, 0xE6, 0x60,
    ]);
    assert_eq!(stop, StopReason::ResetRequested);
}

#[test]
fn guest_d1_with_output_port_bit_0_high_does_not_reset() {
    for value in [0xDF, 0xDD] {
        let (_, stop) = run_reset_probe(&[
            0xB0, 0xD1, 0xE6, 0x64, 0xE4, 0x64, 0xA8, 0x02, 0x75, 0xFA, 0xB0, value, 0xE6, 0x60,
        ]);
        assert_eq!(stop, StopReason::TestExit { code: 0 }, "value {value:#04x}");
    }
}

#[test]
fn guest_port_92h_writes_without_a_rising_bit_0_do_not_reset() {
    for value in [0x02, 0x00] {
        // mov al,value; out 0x92,al
        let (_, stop) = run_reset_probe(&[0xB0, value, 0xE6, 0x92]);
        assert_eq!(stop, StopReason::TestExit { code: 0 }, "value {value:#04x}");
    }
}

#[test]
fn guest_port_92h_bit_0_written_twice_resets_once() {
    // mov al,1; out 0x92,al; out 0x92,al
    let (mut machine, first) = run_reset_probe(&[0xB0, 0x01, 0xE6, 0x92, 0xE6, 0x92]);
    assert_eq!(first, StopReason::ResetRequested);
    // The run returned at the first write. The second write sees the latched
    // bit and does not pulse again, so the program reaches its exit.
    let second = machine.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(second, StopReason::TestExit { code: 0 });
}

#[test]
fn port_92h_reads_back_the_reset_bit_and_the_a20_bit() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    for (written, read) in [(0x03, 0x03), (0x00, 0x00)] {
        with_bus(&mut machine, |bus| {
            bus.write_io(0x92, BusWidth::Byte, written, false).unwrap();
        });
        let value = with_bus(&mut machine, |bus| {
            bus.read_io(0x92, BusWidth::Byte, 0, false).unwrap()
        });
        assert_eq!(value, read, "wrote {written:#04x}");
    }
}

#[test]
fn int16_enhanced_read_returns_f11_f12_codes() {
    assert_eq!(int16_enhanced_read_after(&[0x57, 0xd7]), 0x8500);
    assert_eq!(int16_enhanced_read_after(&[0x58, 0xd8]), 0x8600);
}

#[test]
fn int16_enhanced_read_translates_f11_f12_modifiers() {
    assert_eq!(int16_enhanced_read_after(&[0x2a, 0x57, 0xd7, 0xaa]), 0x8700);
    assert_eq!(int16_enhanced_read_after(&[0x2a, 0x58, 0xd8, 0xaa]), 0x8800);
    assert_eq!(int16_enhanced_read_after(&[0x1d, 0x57, 0xd7, 0x9d]), 0x8900);
    assert_eq!(int16_enhanced_read_after(&[0x1d, 0x58, 0xd8, 0x9d]), 0x8a00);
    assert_eq!(int16_enhanced_read_after(&[0x38, 0x57, 0xd7, 0xb8]), 0x8b00);
    assert_eq!(int16_enhanced_read_after(&[0x38, 0x58, 0xd8, 0xb8]), 0x8c00);
    assert_eq!(
        int16_enhanced_read_after(&[0xe0, 0x38, 0x57, 0xd7, 0xe0, 0xb8]),
        0x8b00
    );
    // Alt wins over Ctrl.
    assert_eq!(
        int16_enhanced_read_after(&[0x1d, 0x38, 0x57, 0xd7, 0xb8, 0x9d]),
        0x8b00
    );
}

#[test]
fn int16_legacy_read_discards_f11_f12() {
    assert_eq!(int16_read_after(&[0x57, 0xd7, 0x1e, 0x9e]), 0x1e61);
    assert_eq!(
        int16_read_after(&[0x2a, 0x58, 0xd8, 0xaa, 0x1e, 0x9e]),
        0x1e61
    );
}

/// Stuff `keys` with INT 16h AH=05h, then run AH=01h and AH=11h, saving AX and
/// the ZF flag of each. Returns (peek AX, peek ZF, enhanced peek AX, enhanced ZF).
fn int16_stuffed_peek(keys: &[u16]) -> (u16, bool, u16, bool) {
    let mut prog: Vec<u8> = Vec::new();
    for key in keys {
        // mov cx,key; mov ah,5; int 16h
        prog.extend_from_slice(&[0xb9, *key as u8, (*key >> 8) as u8, 0xb4, 0x05, 0xcd, 0x16]);
    }
    // mov ah,1; int 16h; mov [0200h],ax; pushf; pop ax; mov [0202h],ax
    prog.extend_from_slice(&[
        0xb4, 0x01, 0xcd, 0x16, 0xa3, 0x00, 0x02, 0x9c, 0x58, 0xa3, 0x02, 0x02,
    ]);
    // mov ah,11h; int 16h; mov [0204h],ax; pushf; pop ax; mov [0206h],ax; int 20h
    prog.extend_from_slice(&[
        0xb4, 0x11, 0xcd, 0x16, 0xa3, 0x04, 0x02, 0x9c, 0x58, 0xa3, 0x06, 0x02, 0xcd, 0x20,
    ]);
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();
    let base = u32::from(DOS_LOAD_SEGMENT) << 4;
    (
        read_u16(&mut machine, base + 0x200),
        read_u16(&mut machine, base + 0x202) & 0x40 != 0,
        read_u16(&mut machine, base + 0x204),
        read_u16(&mut machine, base + 0x206) & 0x40 != 0,
    )
}

#[test]
fn int16_legacy_peek_removes_f11_f12() {
    let (peek_ax, peek_zf, enh_ax, enh_zf) = int16_stuffed_peek(&[0x8500, 0x1e61]);
    assert_eq!(peek_ax, 0x1e61);
    assert!(!peek_zf);
    assert_eq!(enh_ax, 0x1e61, "the legacy peek removed 8500h");
    assert!(!enh_zf);

    let (_, peek_zf, _, enh_zf) = int16_stuffed_peek(&[0x8500]);
    assert!(peek_zf, "only an extended key: legacy peek reports empty");
    assert!(enh_zf, "and the buffer is now empty");
}

#[test]
fn int16_enhanced_peek_keeps_f11() {
    // mov cx,8500h; mov ah,5; int 16h; mov ah,11h; int 16h; mov [0200h],ax;
    // mov ah,10h; int 16h; mov [0202h],ax; int 20h
    const PROG: [u8; 23] = [
        0xb9, 0x00, 0x85, 0xb4, 0x05, 0xcd, 0x16, 0xb4, 0x11, 0xcd, 0x16, 0xa3, 0x00, 0x02, 0xb4,
        0x10, 0xcd, 0x16, 0xa3, 0x02, 0x02, 0xcd, 0x20,
    ];
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &PROG).unwrap();
    machine.run_until_halt_or_cycles(1_000_000).unwrap();
    let base = u32::from(DOS_LOAD_SEGMENT) << 4;
    assert_eq!(read_u16(&mut machine, base + 0x200), 0x8500);
    assert_eq!(read_u16(&mut machine, base + 0x202), 0x8500);
}

#[test]
fn int16_translates_f1_f10_modifiers() {
    let cases: [(&[u8], u16); 14] = [
        (&[0x3b, 0xbb], 0x3b00),
        (&[0x44, 0xc4], 0x4400),
        (&[0x2a, 0x3b, 0xbb, 0xaa], 0x5400),
        (&[0x2a, 0x44, 0xc4, 0xaa], 0x5d00),
        (&[0x1d, 0x3b, 0xbb, 0x9d], 0x5e00),
        (&[0x1d, 0x44, 0xc4, 0x9d], 0x6700),
        (&[0x38, 0x3b, 0xbb, 0xb8], 0x6800),
        (&[0x38, 0x44, 0xc4, 0xb8], 0x7100),
        (&[0x36, 0x3f, 0xbf, 0xb6], 0x5800),
        (&[0xe0, 0x38, 0x3f, 0xbf, 0xe0, 0xb8], 0x6c00),
        (&[0x1d, 0x38, 0x3b, 0xbb, 0xb8, 0x9d], 0x6800),
        (&[0x1d, 0x2a, 0x3b, 0xbb, 0xaa, 0x9d], 0x5e00),
        (&[0x2a, 0x3c, 0xbc, 0xaa], 0x5500),
        (&[0x2a, 0x43, 0xc3, 0xaa], 0x5c00),
    ];
    for (scancodes, expected) in cases {
        assert_eq!(
            int16_read_after(scancodes),
            expected,
            "AH=00h {scancodes:02x?}"
        );
        assert_eq!(
            int16_enhanced_read_after(scancodes),
            expected,
            "AH=10h {scancodes:02x?}"
        );
    }
    assert_eq!(
        int16_read_after_with_layout(2, &[0x2a, 0x3b, 0xbb, 0xaa]),
        0x5400,
        "layout tables do not matter"
    );
}

#[test]
fn int16_gray_arrow_still_differs_between_reads() {
    let up = [0xe0, 0x48, 0xe0, 0xc8];
    assert_eq!(int16_enhanced_read_after(&up), 0x48e0);
    assert_eq!(int16_read_after(&up), 0x4800);
}
