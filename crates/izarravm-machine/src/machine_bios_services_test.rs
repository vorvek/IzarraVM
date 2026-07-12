// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn pci_bios_call(m: &mut Machine, function: u16) -> (u8, bool) {
    prime_dos_int_frame(m);
    m.cpu.registers.set_eax(u32::from(function));
    m.handle_int1a();
    (
        ((m.cpu.registers.eax() >> 8) & 0xff) as u8,
        dos_int_flags(m) & 1 != 0,
    )
}

#[test]
fn pci_bios_installs_finds_and_accesses_every_modeled_function() {
    let mut m = int15_machine(16);
    let (status, carry) = pci_bios_call(&mut m, 0xB101);
    assert_eq!((status, carry), (0, false));
    assert_eq!(m.cpu.registers.edx(), 0x2049_4350);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0210);
    assert_eq!(m.cpu.registers.ecx() as u8, 0);
    assert_eq!(m.cpu.registers.eax() as u8, 1);

    for (vendor, device, occurrence, expected) in [
        (0x8086u16, 0x7110u16, 0u16, 0x0038u16),
        (0x8086, 0x7111, 0, 0x0039),
        (0x121a, 0x0001, 0, 0x0080),
    ] {
        m.cpu.registers.set_edx(u32::from(vendor));
        m.cpu.registers.set_ecx(u32::from(device));
        m.cpu.registers.set_esi(u32::from(occurrence));
        let (status, carry) = pci_bios_call(&mut m, 0xB102);
        assert_eq!((status, carry), (0, false));
        assert_eq!(m.cpu.registers.ebx() as u16, expected);
    }

    m.cpu.registers.set_ecx(0x0001_0180); // IDE class/subclass/interface
    m.cpu.registers.set_esi(0);
    assert_eq!(pci_bios_call(&mut m, 0xB103), (0, false));
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0039);

    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(0);
    assert_eq!(pci_bios_call(&mut m, 0xB10A), (0, false));
    assert_eq!(m.cpu.registers.ecx(), 0x7111_8086);

    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(4);
    m.cpu.registers.set_ecx(0);
    assert_eq!(pci_bios_call(&mut m, 0xB10C), (0, false));
    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(4);
    assert_eq!(pci_bios_call(&mut m, 0xB109), (0, false));
    assert_eq!(m.cpu.registers.ecx() as u16, 0);

    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(4);
    m.cpu.registers.set_ecx(1);
    assert_eq!(pci_bios_call(&mut m, 0xB10B), (0, false));
    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(4);
    assert_eq!(pci_bios_call(&mut m, 0xB108), (0, false));
    assert_eq!(m.cpu.registers.ecx() as u8, 1);

    m.cpu.registers.set_ebx(0x0080);
    m.cpu.registers.set_edi(0x40);
    m.cpu.registers.set_ecx(0xA55A_1234);
    assert_eq!(pci_bios_call(&mut m, 0xB10D), (0, false));
    m.cpu.registers.set_ebx(0x0080);
    m.cpu.registers.set_edi(0x40);
    assert_eq!(pci_bios_call(&mut m, 0xB10A), (0, false));
    assert_eq!(m.cpu.registers.ecx(), 0xA55A_1234);
}

#[test]
fn pci_bios_reports_search_register_and_function_errors() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_edx(0xffff);
    assert_eq!(pci_bios_call(&mut m, 0xB102), (0x83, true));
    m.cpu.registers.set_edx(0x1234);
    m.cpu.registers.set_ecx(0x5678);
    m.cpu.registers.set_esi(0);
    assert_eq!(pci_bios_call(&mut m, 0xB102), (0x86, true));
    m.cpu.registers.set_ebx(0x0039);
    m.cpu.registers.set_edi(3); // unaligned dword
    assert_eq!(pci_bios_call(&mut m, 0xB10A), (0x87, true));
    assert_eq!(pci_bios_call(&mut m, 0xB10E), (0x81, true));
}

#[test]
fn bios32_header_and_far_call_stubs_resolve_pci() {
    let prog = [
        0x66, 0xB8, b'$', b'P', b'C', b'I', // mov eax,'$PCI'
        0x9A, 0x10, 0xEA, 0x00, 0xF0, // call far F000:EA10
        0xEB, 0xFE, // loop
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
    let header = m.read_guest_block(0xFEA00, 16);
    assert_eq!(&header[..4], b"_32_");
    assert_eq!(
        header.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        0
    );
    m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(m.cpu.registers.eax() as u8, 0);
    assert_eq!(m.cpu.registers.ebx(), 0x000F_0000);
    assert_eq!(m.cpu.registers.ecx(), 0x0001_0000);
    assert_eq!(m.cpu.registers.edx(), 0xEA20);

    m.cpu.registers.set_eax(0xB101);
    m.cpu.registers.eflags |= 1;
    m.handle_pci_bios(true);
    assert_eq!(m.cpu.registers.eflags & 1, 0);
    assert_eq!(m.cpu.registers.edx(), 0x2049_4350);
}

#[test]
fn new_raw_program_leaves_pit_counter0_running() {
    // A directly-loaded DOS program must see PIT counter 0 ticking, the way the
    // BIOS POST leaves it; otherwise a guest that polls the timer for a delay or
    // a speed calibration spins forever (TSUMERA's setup does exactly that).
    static PROG: &[u8] = &[0xeb, 0xfe]; // JMP $ - we only need a machine to run.
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), PROG).unwrap();
    fn latched_count(m: &mut Machine) -> u16 {
        let mut bus = m.make_bus();
        bus.write_io(0x43, BusWidth::Byte, 0x00, false).unwrap(); // latch counter 0
        let lo = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        let hi = bus.read_io(0x40, BusWidth::Byte, 0, false).unwrap() as u16;
        lo | (hi << 8)
    }
    let before = latched_count(&mut m);
    m.run_until_halt_or_cycles(100_000).unwrap();
    let after = latched_count(&mut m);
    assert_ne!(
        before, after,
        "PIT counter 0 must advance after new_raw_program (POST-equivalent timer setup)"
    );
}

#[test]
fn new_raw_program_runs_and_exits_via_int20() {
    let prog: &[u8] = &[0xcd, 0x20]; // int 20h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
}

#[test]
fn new_raw_program_exits_with_ah4c_code() {
    let prog: &[u8] = &[0xb8, 0x2a, 0x4c, 0xcd, 0x21]; // mov ax,4c2a; int 21h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0x2a });
}

#[test]
fn raw_program_profile_records_cpu_batch_phase() {
    let prog: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21h
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    m.enable_host_profiling(1);

    let reason = m.run_until_halt_or_cycles(100_000).unwrap();

    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let host = m.host_profile_snapshot();
    let cpu_batch = host
        .phases
        .iter()
        .find(|phase| phase.name == "cpu_batch")
        .expect("cpu_batch phase exists");
    assert!(cpu_batch.count > 0, "CPU batches should be counted");
    assert!(
        cpu_batch.wall_ns > 0,
        "CPU batch wall time should be measured"
    );
    let cpu = m.cpu().profile_snapshot();
    assert!(
        cpu.groups.iter().any(|bucket| bucket.instructions > 0),
        "CPU group profile should record retired instructions"
    );
}

#[test]
fn raw_program_uses_direct_page_data_and_fetch_caches() {
    let mut prog = vec![
        0xb9, 0x20, 0x00, // mov cx,32
        0xa1, 0x20, 0x01, // loop: mov ax,[0120h]
        0xa3, 0x22, 0x01, // mov [0122h],ax
        0xe2, 0xf8, // loop loop
        0xcd, 0x20, // int 20h
    ];
    prog.resize(0x20, 0);
    prog.extend_from_slice(&0xBEEFu16.to_le_bytes());
    prog.extend_from_slice(&0u16.to_le_bytes());

    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &prog).unwrap();
    m.cpu.reset_perf_counters();
    let reason = m.run_until_halt_or_cycles(1_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let data_addr = (u32::from(DOS_LOAD_SEGMENT) << 4) + 0x0122;
    assert_eq!(
        m.memory.read_u16(data_addr as usize).unwrap(),
        0xBEEF,
        "the loop copied the direct-read word to the direct-write slot"
    );
    let perf = m.cpu.perf_counters();
    assert!(
        perf.direct_data_pointer_reads > 0,
        "scalar RAM reads should use cached page pointers"
    );
    assert!(
        perf.direct_data_pointer_writes > 0,
        "scalar RAM writes should use cached page pointers"
    );
    assert!(
        perf.fetch_page_hits > 0,
        "instruction decode should hit the direct fetch page"
    );
    assert_eq!(
        perf.slow_prefetch_refills, 0,
        "RAM instruction fetch should not need copied prefetch refills"
    );
}

#[test]
fn new_raw_program_prints_a_dollar_terminated_string() {
    // org 0x100: mov ah,9 / mov dx,msg / int 21h / mov ax,4c00h / int 21h
    // msg ("Hi$") placed right after the code, addressed PSP-relative.
    // Code is 12 bytes, so msg starts at offset 0x100+12 = 0x10C.
    let prog: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(m.program_output(), b"Hi");
}

#[test]
fn new_raw_program_output_reaches_the_vga_screen() {
    // Same program as new_raw_program_prints_a_dollar_terminated_string:
    // org 0x100: mov ah,9 / mov dx,msg / int 21h / mov ax,4c00h / int 21h
    // msg ("Hi$") placed right after the code, addressed PSP-relative.
    // Code is 12 bytes, so msg starts at offset 0x100+12 = 0x10C.
    let prog: &[u8] = &[
        0xb4, 0x09, 0xba, 0x0c, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21, b'H', b'i', b'$',
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    let reason = m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    let screen = m.screen_text();
    assert!(
        screen.line_string(0).starts_with("Hi"),
        "screen line 0 was {:?}",
        screen.line_string(0)
    );
}

#[test]
fn new_raw_program_reads_typed_keys_via_ah01() {
    // org 0x100: mov ah,1 / int 21h / mov ah,1 / int 21h / mov ax,4c00h / int 21h
    let prog: &[u8] = &[
        0xb4, 0x01, 0xcd, 0x21, 0xb4, 0x01, 0xcd, 0x21, 0xb8, 0x00, 0x4c, 0xcd, 0x21,
    ];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    m.set_program_stdin(b"hi");
    let reason = m.run_until_halt_or_cycles(2_000_000).unwrap();
    assert_eq!(reason, StopReason::DosExit { code: 0 });
    assert_eq!(m.program_output(), b"hi");
}

#[test]
fn new_raw_program_unknown_int21_function_sets_carry() {
    // org 0x100: mov ah,0xff / int 21h ; the unrecognized AH=FFh falls
    // into a tight loop on CF so the test can stop and inspect FLAGS
    // without the program continuing past it.
    let prog: &[u8] = &[0xb4, 0xff, 0xcd, 0x21, 0xeb, 0xfe];
    let mut m =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    m.run_until_halt_or_cycles(1_000).unwrap();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0007);
    assert_eq!(m.cpu.registers.eflags & 0x0001, 0x0001, "CF set");
}

#[test]
fn new_raw_program_seeds_env_one_paragraph_above_prog_top() {
    let prog: &[u8] = &[0xb8, 0x00, 0x4c, 0xcd, 0x21];
    let m = Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), prog).unwrap();
    let prog_top = m
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 2)
        .unwrap();
    let env_seg = m
        .memory()
        .read_u16(usize::from(DOS_LOAD_SEGMENT) * 16 + 0x2c)
        .unwrap();
    assert_eq!(env_seg, prog_top + 1);
}

#[test]
fn int15_8a_reports_extended_memory_as_dx_ax() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0x8A00);
    m.handle_int15();
    // 23 MB above the first 1 MB = 23552 KB = 0x5C00 (fits in AX, DX = 0).
    assert_eq!(m.cpu.registers.eax() as u16, 0x5C00);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000);
}

#[test]
fn int15_21_post_error_log_stores_and_reads_entries() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x2101);
    m.cpu.registers.set_ebx(0x1234); // BH=device, BL=error
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "write AH=0");
    assert_eq!(dos_int_flags(&m) & 1, 0, "write CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x2100);
    m.cpu.registers.set_edi(0xCAFE_0000);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "read AH=0");
    assert_eq!(m.cpu.registers.ebx() as u16, 1, "one POST record");
    let es = m.cpu.registers.segment(SegmentIndex::Es).base;
    let di = m.cpu.registers.edi() as u16;
    assert_eq!(es + u32::from(di), BIOS_POST_ERROR_LOG_ADDR);
    assert_eq!(m.read_physical_u8(BIOS_POST_ERROR_LOG_ADDR), 0x34);
    assert_eq!(m.read_physical_u8(BIOS_POST_ERROR_LOG_ADDR + 1), 0x12);
}

#[test]
fn int15_83_event_wait_sets_completion_byte() {
    let mut m = int15_machine(16);
    m.write_physical_u8(0x4_0000, 0x01);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8300);
    m.cpu.registers.set_ecx(0x0000);
    m.cpu.registers.set_edx(0x0001);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4000));
    m.cpu.registers.set_ebx(0x0000);
    m.handle_int15();

    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0");
    assert_eq!(m.read_physical_u8(0x4_0000), 0x81, "completion bit set");
    assert_eq!(dos_int_flags(&m) & 1, 0, "CF clear");
}

#[test]
fn int15_84_reports_absent_joystick() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x84FF);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "switches open");
    assert_eq!(dos_int_flags(&m) & 1, 0, "switch read CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8400);
    m.cpu.registers.set_ebx(0xFFFF);
    m.cpu.registers.set_ecx(0xFFFF);
    m.cpu.registers.set_edx(0x0001);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000, "joy A X");
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000, "joy A Y");
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000, "joy B X");
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000, "joy B Y");
    assert_eq!(dos_int_flags(&m) & 1, 0, "position read CF clear");
}

#[test]
fn int15_reports_absent_cassette() {
    for ah in [0x00u8, 0x01, 0x02, 0x03] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(u32::from(ah) << 8);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AH={ah:02X}");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AH={ah:02X} CF set");
    }
}

#[test]
fn int15_keyboard_intercept_continues_scan_code() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x4F1E);
    m.handle_int15();

    assert_eq!(m.cpu.registers.eax() as u8, 0x1E, "scan code preserved");
    assert_eq!(dos_int_flags(&m) & 1, 1, "CF set continues processing");
}

#[test]
fn int15_os_device_hooks_succeed_as_noops() {
    for ah in [0x80u8, 0x81, 0x82] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax((u32::from(ah) << 8) | 0x55);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH={ah:02X}");
        assert_eq!(dos_int_flags(&m) & 1, 0, "AH={ah:02X} CF clear");
    }
}

#[test]
fn int15_reports_absent_watchdog_and_pos() {
    for ax in [0xC300u32, 0xC400] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(ax);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AX={ax:04X}");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AX={ax:04X} CF set");
    }
}

#[test]
fn int15_reports_absent_window_manager_print_and_convertible_calls() {
    for ax in [
        0x1000u32, 0x1022, 0x102D, 0xDE00, 0xDE12, 0x1100, 0x1200, 0x2000, 0x4000, 0x4400,
    ] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0xFFFF);
        m.handle_int15();

        assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x86, "AX={ax:04X}");
        assert_eq!(m.cpu.registers.ebx() as u16, 0x0000, "AX={ax:04X} BX");
        assert_eq!(dos_int_flags(&m) & 1, 1, "AX={ax:04X} CF set");
    }
}

#[test]
fn int15_low_bios_hooks_return_defined_status() {
    let mut m = int15_machine(16);

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0F02);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "format continues");
    assert_eq!(dos_int_flags(&m) & 1, 0, "AH=0F CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8500);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "SysReq hook OK");
    assert_eq!(dos_int_flags(&m) & 1, 0, "AH=85 CF clear");

    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x8900);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x86,
        "BIOS protected-mode switch unsupported"
    );
    assert_eq!(dos_int_flags(&m) & 1, 1, "AH=89 CF set");
}

#[test]
fn int1a_09_reports_alarm_disabled() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0x0900);
    m.cpu.registers.set_ecx(0xFFFF);
    m.cpu.registers.set_edx(0xFFFF);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000, "alarm time");
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000, "alarm disabled");
    assert_eq!(dos_int_flags(&m) & 1, 0, "CF clear");
}

#[test]
fn int1a_80_sound_multiplexor_is_iret_noop() {
    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.memory.write_u16(0x9000 * 16 + 0x0104, 0x0241).unwrap();
    m.cpu.registers.set_eax(0x8055);
    m.cpu.registers.set_ebx(0x1234);
    m.cpu.registers.set_ecx(0x5678);
    m.cpu.registers.set_edx(0x9ABC);

    m.handle_int1a();

    assert_eq!(m.cpu.registers.eax() as u16, 0x8055, "AX preserved");
    assert_eq!(m.cpu.registers.ebx() as u16, 0x1234, "BX preserved");
    assert_eq!(m.cpu.registers.ecx() as u16, 0x5678, "CX preserved");
    assert_eq!(m.cpu.registers.edx() as u16, 0x9ABC, "DX preserved");
    assert_eq!(dos_int_flags(&m), 0x0241, "FLAGS image preserved");
}

#[test]
fn int15_e801_splits_memory_at_16m() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0xE801);
    m.handle_int15();
    // 1-16 MB capped at 0x3C00 KB; 8 MB above 16 MB = 128 64KB-blocks = 0x80.
    assert_eq!(m.cpu.registers.eax() as u16, 0x3C00);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x80);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x3C00);
    assert_eq!(m.cpu.registers.edx() as u16, 0x80);
}

#[test]
fn int15_e820_walks_the_memory_map() {
    let mut m = int15_machine(24);
    // ES = 0, DI = 0: the descriptor lands at physical 0 in test RAM.
    let mut ebx = 0u32;
    let mut regions = Vec::new();
    loop {
        m.cpu.registers.set_eax(0xE820);
        m.cpu.registers.set_edx(0x534D_4150);
        m.cpu.registers.set_ecx(20);
        m.cpu.registers.set_ebx(ebx);
        m.handle_int15();
        assert_eq!(m.cpu.registers.eax(), 0x534D_4150);
        assert_eq!(m.cpu.registers.ecx(), 20);
        let base = m.read_guest_dword(0);
        let len = m.read_guest_dword(8);
        let kind = m.read_guest_dword(16);
        regions.push((base, len, kind));
        ebx = m.cpu.registers.ebx();
        if ebx == 0 {
            break;
        }
    }
    assert_eq!(regions.len(), 4);
    assert_eq!(regions[0], (0x0, 0x9_FC00, 1)); // 639 KB conventional (below EBDA)
    assert_eq!(regions[1], (0x9_FC00, 0x400, 2)); // 1 KB EBDA, reserved
    assert_eq!(regions[2], (0xA_0000, 0x6_0000, 2)); // reserved hole
    assert_eq!(regions[3], (0x10_0000, 23 * 0x10_0000, 1)); // extended RAM
}

#[test]
fn int15_c201_reset_reports_present_standard_mouse() {
    // C201 resets the PS/2 mouse: BH=0x00 (standard device id), BL=0xAA (the
    // reset-complete signature drivers probe for), AH=0x00, CF clear.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC201);
    m.cpu.registers.set_ebx(0xFFFF);
    m.handle_int15();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x00AA, "BH=00 BL=AA");
    assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
}

#[test]
fn int15_c204_reports_standard_device_type() {
    // C204 get device type: BH=0x00 (standard PS/2 mouse), AH=0x00.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC204);
    m.cpu.registers.set_ebx(0xFF00);
    m.handle_int15();
    assert_eq!((m.cpu.registers.ebx() as u16 >> 8) as u8, 0x00, "BH=00");
    assert_eq!((m.cpu.registers.eax() as u16 >> 8) as u8, 0x00, "AH=00");
}

#[test]
fn int15_c206_status_describes_an_enabled_mouse() {
    // C206 BH=00 returns the three status bytes. BL bit5 = mouse enabled.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC206);
    m.cpu.registers.set_ebx(0x0000); // BH=00
    m.handle_int15();
    assert_eq!(m.cpu.registers.ebx() as u8 & 0x20, 0x20, "BL bit5 enabled");
}

#[test]
fn int15_c207_set_handler_stores_pointer_and_succeeds() {
    // C207 (set device handler) registers the ES:BX far pointer in the EBDA and
    // returns success (AH=0, CF clear). The stored pointer is the one the BIOS
    // INT 74h ISR far-calls on each completed PS/2 packet.
    let mut m = int15_machine(16);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0xABCD));
    m.cpu.registers.set_ebx(0x0042);
    m.cpu.registers.set_eax(0xC207);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() as u16 >> 8) as u8,
        0x00,
        "AH=0 success"
    );
    let base = (u32::from(EBDA_SEGMENT) << 4) + EBDA_MOUSE_HANDLER_OFF;
    assert_eq!(read_u16(&mut m, base), 0x0042);
    assert_eq!(read_u16(&mut m, base + 2), 0xABCD);
}

#[test]
fn int15_c208_still_reports_unsupported() {
    // C208 (read raw device port) has no wired path: AH=0x86 unsupported.
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xC208);
    m.handle_int15();
    assert_eq!(
        (m.cpu.registers.eax() as u16 >> 8) as u8,
        0x86,
        "AH=86 unsupported"
    );
}

#[test]
fn int15_e820_rejects_a_bad_smap_signature() {
    let mut m = int15_machine(24);
    m.cpu.registers.set_eax(0xE820);
    m.cpu.registers.set_edx(0); // not 'SMAP'
    m.cpu.registers.set_ecx(20);
    m.handle_int15();
    // EAX must not be rewritten to 'SMAP' when the call is rejected.
    assert_ne!(m.cpu.registers.eax(), 0x534D_4150);
}

#[test]
fn int14_status_reports_uart_registers() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0300); // AH=03h read status
    m.cpu.registers.set_edx(0); // COM1
    m.handle_int14();
    // LSR reads 0x60 (THRE|TEMT) on the idle UART; MSR reads 0x00.
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x60,
        "line status in AH"
    );
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "modem status in AL");
}

#[test]
fn int14_send_writes_a_byte_to_the_uart() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0158); // AH=01h send AL='X'
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(
        m.serial.output(),
        b"X",
        "byte reached the UART capture sink"
    );
    // THRE is always set, so the send succeeds with bit7 clear.
    assert_eq!((m.cpu.registers.eax() >> 8) as u8 & 0x80, 0, "no timeout");
}

#[test]
fn int14_extended_initialize_programs_uart_format_and_divisor() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0401); // AH=04h, no break
    m.cpu.registers.set_ebx(0x0201); // even parity, two stop bits
    m.cpu.registers.set_ecx(0x0308); // 8 data bits, 19200 baud
    m.cpu.registers.set_edx(0); // COM1
    m.handle_int14();

    let lcr = m.serial.read_port(0x03fb).unwrap();
    assert_eq!(lcr, 0x1f, "8E2 line format");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x60, "LSR in AH");
    m.serial.write_port(0x03fb, lcr | 0x80); // DLAB on
    assert_eq!(m.serial.read_port(0x03f8).unwrap(), 6, "DLL for 19200");
    assert_eq!(m.serial.read_port(0x03f9).unwrap(), 0, "DLM for 19200");
    m.serial.write_port(0x03fb, lcr);
}

#[test]
fn int14_modem_control_read_write_round_trips() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0501); // AH=05h, AL=01h write MCR
    m.cpu.registers.set_ebx(0x0013); // DTR|RTS|LOOP
    m.cpu.registers.set_edx(0);
    m.handle_int14();

    assert_eq!(m.serial.read_port(0x03fc).unwrap(), 0x13);

    m.cpu.registers.set_eax(0x0500); // AH=05h, AL=00h read MCR
    m.cpu.registers.set_ebx(0xAA00);
    m.cpu.registers.set_edx(0);
    m.handle_int14();

    assert_eq!(m.cpu.registers.ebx() as u16, 0xAA13);
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
}

#[test]
fn int14_unwired_port_times_out() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0300);
    // INT 14h only services COM1 (DX=0); the COM2 hardware exists but the
    // BIOS service does not drive it, so DX=1 reads as a timeout.
    m.cpu.registers.set_edx(1);
    m.handle_int14();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8 & 0x80,
        0x80,
        "timeout bit set"
    );
}

#[test]
fn int14_fossil_services_use_uart_and_bios_state() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x0601);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_ne!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR raised");

    m.cpu.registers.set_eax(0x0600);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR lowered");

    m.cpu.registers.set_eax(0x0400);
    m.cpu.registers.set_ebx(0x4F50);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x1954);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x001B);
    assert_ne!(m.serial.read_port(0x03fc).unwrap() & 0x01, 0, "DTR raised");

    m.cpu.registers.set_eax(0x0B58);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0001);
    assert_eq!(m.serial.output(), b"X");

    m.write_guest_block(0x4000, b"yz");
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x400));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(2);
    m.cpu.registers.set_eax(0x1900);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 2);
    assert_eq!(m.serial.output(), b"Xyz");

    m.serial.write_port(0x03fc, 0x10);
    m.serial.write_port(0x03f8, b'R');
    m.advance_devices_ticks(m.serial.ticks_until_idle());
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x500));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(4);
    m.cpu.registers.set_eax(0x1800);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 1);
    assert_eq!(m.read_physical_u8(0x5000), b'R');

    m.set_program_stdin(b"k");
    m.cpu.registers.set_eax(0x0D00);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, b'k' as u16);
}

#[test]
fn int14_fossil_screen_and_info_calls_are_minimal_but_stable() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_edx(0x0407);
    m.cpu.registers.set_eax(0x1100);
    m.handle_int14();
    assert_eq!(m.read_guest_word(0x450), 0x0407);

    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_eax(0x1200);
    m.handle_int14();
    assert_eq!(m.cpu.registers.edx() as u16, 0x0407);

    m.cpu.registers.set_eax(0x1541);
    m.handle_int14();
    let cell = (4 * 80 + 7) * 2;
    assert_eq!(m.video().read_u8(cell).unwrap(), b'A');

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x600));
    m.cpu.registers.set_edi(0);
    m.cpu.registers.set_ecx(21);
    m.cpu.registers.set_eax(0x1B00);
    m.cpu.registers.set_edx(0);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 21);
    assert_eq!(m.memory.read_u16(0x6000).unwrap(), 21);
    assert_eq!(m.read_physical_u8(0x6002), 5);
    assert_eq!(m.read_physical_u8(0x6010), 80);
    assert_eq!(m.read_physical_u8(0x6011), 25);

    m.cpu.registers.set_eax(0x7E42);
    m.cpu.registers.set_ebx(0);
    m.cpu.registers.set_edx(0x1234);
    m.handle_int14();
    assert_eq!(m.cpu.registers.eax() as u16, 0x1954);
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0042);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0034);
}

#[test]
fn int17_print_captures_and_reports_ready() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0050); // AH=00h print AL='P'
    m.cpu.registers.set_edx(0); // LPT1
    m.handle_int17();
    assert_eq!(m.lpt_output(), b"P", "byte reached the LPT capture sink");
    // An always-ready printer reports 0x90: not busy, selected, no error/timeout.
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x90,
        "ready status in AH"
    );
}

#[test]
fn int17_status_reports_ready_printer() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0200); // AH=02h read status
    m.cpu.registers.set_edx(0);
    m.handle_int17();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x90,
        "ready status in AH"
    );
    assert!(m.lpt_output().is_empty(), "status query prints nothing");
}

#[test]
fn bda_seeds_serial_and_parallel_port_bases() {
    let m = int15_machine(16);
    assert_eq!(
        m.memory.read_u16(0x400).unwrap(),
        0x03f8,
        "COM1 base at 0040:0000"
    );
    assert_eq!(
        m.memory.read_u16(0x408).unwrap(),
        0x0378,
        "LPT1 base at 0040:0008"
    );
}

#[test]
fn int15_a20_status_enable_and_disable() {
    let mut m = int15_machine(16);
    // The 8042 output port defaults to A20 on, so status reads enabled.
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    assert_eq!(m.cpu.registers.eax() as u8, 0x01, "A20 enabled by default");
    // AH=2400h disable.
    m.cpu.registers.set_eax(0x2400);
    m.handle_int15();
    assert!(
        !m.keyboard.a20_enabled(),
        "8042 A20 state off after disable"
    );
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!(m.cpu.registers.eax() as u8, 0x00, "status reports disabled");
    // AH=2401h enable.
    m.cpu.registers.set_eax(0x2401);
    m.handle_int15();
    assert!(m.keyboard.a20_enabled(), "8042 A20 state on after enable");
}

#[test]
fn int15_a20_query_support_reports_both_methods() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x2403);
    m.handle_int15();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH=0 success");
    // Bit 0 keyboard controller, bit 1 port 0x92.
    assert_eq!(
        m.cpu.registers.ebx() as u16,
        0x0003,
        "both A20 methods supported"
    );
}

#[test]
fn port_92_and_int15_a20_stay_coherent() {
    let mut m = int15_machine(16);
    // Disable A20 through the fast-A20 port; it reads back off.
    {
        let mut bus = m.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0x00, false).unwrap();
        assert_eq!(
            bus.read_io(0x0092, BusWidth::Byte, 0, false).unwrap(),
            0x00,
            "port 0x92 A20 off"
        );
    }
    assert!(!m.keyboard.a20_enabled(), "8042 agrees A20 is off");
    m.cpu.registers.set_eax(0x2402);
    m.handle_int15();
    assert_eq!(
        m.cpu.registers.eax() as u8,
        0x00,
        "INT 15h status agrees A20 is off"
    );
    // Enable through the port again; bit 1 reads back set.
    {
        let mut bus = m.make_bus();
        bus.write_io(0x0092, BusWidth::Byte, 0x02, false).unwrap();
        assert_eq!(
            bus.read_io(0x0092, BusWidth::Byte, 0, false).unwrap(),
            0x02,
            "port 0x92 A20 on"
        );
    }
    assert!(m.keyboard.a20_enabled(), "8042 agrees A20 is on");
}

#[test]
fn a20_toggle_through_the_run_loop_invalidates_the_decode_cache() {
    // End-to-end check of the A20 -> decode-cache seam: a guest OUT to port 0x92, executed by
    // the real run loop, must advance the CPU's decode generation (so a wrap-region cached
    // decode is dropped). The control program -- identical but a NOP instead of the OUT -- must
    // not advance it, proving the bump comes from the A20 toggle and not incidental run-loop
    // activity. Both spin on JMP $ so the short run never reaches a HLT or a timer interrupt.
    fn gen_after_running(program: &[u8]) -> (bool, u32, u32) {
        let mut m = Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), program)
            .unwrap();
        let before = m.cpu.decode_cache_generation();
        m.run_until_halt_or_cycles(1000).unwrap();
        (
            m.keyboard.a20_enabled(),
            before,
            m.cpu.decode_cache_generation(),
        )
    }

    // MOV AL, 0; OUT 0x92, AL; JMP $  -- drives A20 off (port 0x92 bit 1 = 0).
    let (a20, before, after) = gen_after_running(&[0xb0, 0x00, 0xe6, 0x92, 0xeb, 0xfe]);
    assert!(
        !a20,
        "the guest OUT 0x92 toggled A20 off through the run loop"
    );
    assert_ne!(
        after, before,
        "the A20 toggle advanced the decode generation (note_a20_changed fired)"
    );

    // MOV AL, 0; NOP; JMP $  -- no port write, so A20 stays on and the generation is steady.
    let (a20, before, after) = gen_after_running(&[0xb0, 0x00, 0x90, 0xeb, 0xfe]);
    assert!(a20, "control: A20 stays on");
    assert_eq!(
        after, before,
        "control: no A20 toggle, so the decode generation is unchanged by the run"
    );
}

#[test]
fn a20_off_folds_the_hma_onto_low_memory() {
    let mut m = int15_machine(16);
    // A20 is on by default, so 0x0 and 0x100000 are distinct cells.
    {
        let mut bus = m.make_bus();
        bus.write_memory(0x0, BusWidth::Byte, 0xAA, BusAccessKind::DataWrite)
            .unwrap();
        bus.write_memory(0x10_0000, BusWidth::Byte, 0xBB, BusAccessKind::DataWrite)
            .unwrap();
        assert_eq!(
            bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xBB,
            "a distinct extended cell with A20 on"
        );
    }
    // Close the gate: a write to 0x100000 now folds onto 0x0.
    m.keyboard.set_a20(false);
    {
        let mut bus = m.make_bus();
        bus.write_memory(0x10_0000, BusWidth::Byte, 0xCC, BusAccessKind::DataWrite)
            .unwrap();
        assert_eq!(
            bus.read_memory(0x0, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xCC,
            "the HMA write reached 0x0 through the closed gate"
        );
    }
    // Reopen the gate: the real extended cell was never touched (still 0xBB).
    m.keyboard.set_a20(true);
    {
        let mut bus = m.make_bus();
        assert_eq!(
            bus.read_memory(0x10_0000, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xBB,
            "the aliased write left the extended cell alone"
        );
    }
}

#[test]
fn unoccupied_upper_memory_reads_open_bus() {
    // 0xC8000-0xEFFFF are the UMB-able holes above the VGA option ROM span
    // and below the system BIOS. Nothing on this machine's default boot
    // claims them, so a probe (JEMMEX and other EMS/UMB managers scan the
    // UMA for a free page frame) must see open bus, not RAM that happens
    // to hold whatever was last written there.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    for addr in [0xC8000u32, 0xC8001, 0xE0000, 0xEFFFF] {
        assert_eq!(
            bus.read_memory(addr, BusWidth::Byte, BusAccessKind::DataRead)
                .unwrap(),
            0xff,
            "address {addr:#08x} must read open bus"
        );
    }
    // A write finds nothing wired to receive it: read-back still 0xFF.
    bus.write_memory(0xD0000, BusWidth::Byte, 0x42, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xD0000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xff,
        "an open-bus write must not stick"
    );
    // The occupied VGA BIOS span (0xC0000-0xC7FFF) is unaffected: it is
    // genuinely backed and keeps its written content.
    bus.write_memory(0xC5000, BusWidth::Byte, 0x99, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xC5000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0x99,
        "the VGA BIOS span is still flat-RAM-backed, not open bus"
    );
    // The system BIOS ROM shadow at 0xF0000 is unaffected: a write is a
    // silent no-op (ROM), not open bus 0xFF read-back of arbitrary content.
    let before = bus
        .read_memory(0xF0000, BusWidth::Byte, BusAccessKind::DataRead)
        .unwrap();
    bus.write_memory(0xF0000, BusWidth::Byte, !before, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0xF0000, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        before,
        "the BIOS ROM shadow ignores writes and keeps its ROM content"
    );
}

#[test]
fn a20_off_folds_a_split_word_in_the_hma() {
    let mut m = int15_machine(16);
    m.keyboard.set_a20(false);
    let mut bus = m.make_bus();
    // 0x100001 is odd, so the word splits; with the gate closed each byte
    // folds down by 0x100000, landing the pair at 0x1 and 0x2. (The byte just
    // below 1 MiB, 0xFFFFF, is BIOS ROM, so the genuinely straddling write is
    // not observable there; the odd HMA word proves the same split masking.)
    bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
        .unwrap();
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xEF,
        "low byte folded to 0x1"
    );
    assert_eq!(
        bus.read_memory(0x2, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xBE,
        "high byte folded to 0x2"
    );
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
            .unwrap(),
        0xBEEF,
        "the folded word reads back through the HMA alias"
    );
}

#[test]
fn a20_off_folds_a_split_dword_and_reads_back() {
    let mut m = int15_machine(16);
    m.keyboard.set_a20(false);
    let mut bus = m.make_bus();
    // 0x100001 is not 4-aligned, so the dword splits into four bytes, each
    // folding down by 0x100000 to 0x1..0x4.
    bus.write_memory(
        0x10_0001,
        BusWidth::Dword,
        0xDEAD_BEEF,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    // The read side folds too: the dword reads back through the alias.
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap(),
        0xDEAD_BEEF,
        "the dword reads back through the HMA alias"
    );
    // The low-memory bytes hold the little-endian image.
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xEF,
        "byte 0 folded to 0x1"
    );
    assert_eq!(
        bus.read_memory(0x4, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        0xDE,
        "byte 3 folded to 0x4"
    );
}

#[test]
fn a20_on_keeps_a_split_word_in_the_hma() {
    let mut m = int15_machine(16); // A20 on by default
    let mut bus = m.make_bus();
    bus.write_memory(0x10_0001, BusWidth::Word, 0xBEEF, BusAccessKind::DataWrite)
        .unwrap();
    // Low memory is untouched; the word stays at the real HMA cells. Byte
    // 0x1 is IVT[0]'s offset high byte, seeded to the per-vector ROM stub
    // (bios_int_stub_off(0) = 0x0200 -> high byte 0x02).
    assert_eq!(
        bus.read_memory(0x1, BusWidth::Byte, BusAccessKind::DataRead)
            .unwrap(),
        u32::from(bios_int_stub_off(0) >> 8),
        "0x1 untouched with A20 on"
    );
    assert_eq!(
        bus.read_memory(0x10_0001, BusWidth::Word, BusAccessKind::DataRead)
            .unwrap(),
        0xBEEF,
        "the word stayed in the HMA"
    );
}

#[test]
fn int2f_idle_yield_reports_supported() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1680);

    assert!(m.handle_int2f(), "AX=1680h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1600);
}

#[test]
fn int2f_windows_install_probe_reports_plain_dos() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1600);
    m.cpu.registers.set_ebx(0x1111_2222);

    assert!(m.handle_int2f(), "AX=1600h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1600);
    assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
}

#[test]
fn int2f_dpmi_probes_report_absent() {
    for ax in [0x1686u16, 0x1687] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
    }
}

#[test]
fn int2f_dos_install_probes_report_not_installed() {
    for (ax, name) in [
        (0x0100u16, "PRINT"),
        (0x0500, "critical-error helper"),
        (0x0600, "ASSIGN"),
        (0x1000, "SHARE"),
        (0x1400, "NLSFUNC"),
        (0x2300, "DR DOS GRAFTABL"),
        (0x2E00, "Novell GRAFTABL"),
        (0x6400, "SCRNSAV2"),
        (0x7A00, "NetWare"),
        (0xAA00, "VIDCLOCK"),
        (0xAD00, "DISPLAY.SYS"),
        (0xB000, "GRAFTABL"),
        (0xB700, "APPEND"),
        (0xF700, "AUTOPARK"),
    ] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);
        m.cpu.registers.set_ecx(0x3333_4444);
        m.cpu.registers.set_edx(0x5555_6666);

        assert!(m.handle_int2f(), "{name} install check handled");

        assert_eq!(m.cpu.registers.eax() as u8, 0x00, "{name} not installed");
        if matches!(ax, 0x0600 | 0x2300 | 0x2E00 | 0xB700) {
            assert_eq!(
                (m.cpu.registers.eax() as u16) >> 8,
                0x00,
                "{name} also clears AH"
            );
        }
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        assert_eq!(m.cpu.registers.ecx(), 0x3333_4444);
        assert_eq!(m.cpu.registers.edx(), 0x5555_6666);
    }

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_B800);
    m.cpu.registers.set_ebx(0x1111_2222);

    assert!(m.handle_int2f(), "network install check handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000);
    assert_eq!(m.cpu.registers.ebx(), 0x1111_0000);

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_0601);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3333));

    assert!(m.handle_int2f(), "ASSIGN work-area query handled");

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);

    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_B803);
    m.cpu.registers.set_ebx(0xAAAA_5555);

    assert!(m.handle_int2f(), "network post-address read handled");

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);
    assert_eq!(m.cpu.registers.ebx(), 0xAAAA_0000);

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x4567));
    m.cpu.registers.set_ebx(0xBBBB_1234);
    m.cpu.registers.set_eax(0xCAFE_B804);

    assert!(m.handle_int2f(), "network post-address set handled");

    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0));
    m.cpu.registers.set_ebx(0xCCCC_0000);
    m.cpu.registers.set_eax(0xCAFE_B803);

    assert!(
        m.handle_int2f(),
        "network post-address read after set handled"
    );

    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x4567);
    assert_eq!(m.cpu.registers.ebx(), 0xCCCC_1234);

    for ax in 0x0101u16..=0x0105 {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "PRINT AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(dos_int_flags(&m) & 1, 0, "PRINT service sets CF");
    }

    for ax in [0x0501u16, 0x05ff] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "critical-error AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(
            dos_int_flags(&m) & 1,
            0,
            "critical-error helper service sets CF"
        );
    }

    for ax in [0x1401u16, 0x1402, 0x1403, 0x1404, 0x14FE, 0x14FF] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "NLSFUNC AX={ax:04X}h handled");

        assert_eq!(
            m.cpu.registers.eax(),
            0xCAFE_1401,
            "absent NLSFUNC service reports DOS error 1 in AL"
        );
    }

    for ax in [0xB001u16, 0x2301, 0x2E01] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "GRAFTABL AX={ax:04X}h handled");

        if ax == 0xB001 {
            assert_eq!(
                m.cpu.registers.eax() as u8,
                0x00,
                "MS-DOS GRAFTABL data call does not claim a font table"
            );
        } else {
            assert_eq!(
                (m.cpu.registers.eax() as u16) >> 8,
                0x00,
                "DR/Novell GRAFTABL data call reports not installed"
            );
        }
    }

    for ax in [0xB701u16, 0xB702, 0xB809, 0xF701] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "AX={ax:04X}h absent service handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_ne!(dos_int_flags(&m) & 1, 0, "absent service sets CF");
    }
}

#[test]
fn int2f_absent_redirector_calls_fail_or_noop() {
    for ax in [
        0x1101u16, 0x1102, 0x1103, 0x1104, 0x1105, 0x1106, 0x1107, 0x1108, 0x1109, 0x110A, 0x110B,
        0x110C, 0x110D, 0x110E, 0x110F, 0x1110, 0x1111, 0x1112, 0x1113, 0x1114, 0x1115, 0x1116,
        0x1117, 0x1118, 0x1119, 0x111A, 0x111B, 0x111C, 0x111E, 0x111F, 0x1121, 0x1123, 0x1124,
        0x1125, 0x1126, 0x1127, 0x1128, 0x1129, 0x112A, 0x112B, 0x112C, 0x112D, 0x112E, 0x112F,
    ] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0001);
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
        let sp = m.cpu.registers.esp() as u16;
        let flags = m
            .memory
            .read_u16((ss + u32::from(sp.wrapping_add(4))) as usize)
            .unwrap();
        assert_ne!(flags & 0x0001, 0, "CF set");
    }

    for ax in [0x111Du16, 0x1122] {
        let mut m = int15_machine(16);
        prime_dos_int_frame(&mut m);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000 | u32::from(ax));
        assert_ne!(dos_int_flags(&m) & 1, 0, "notify hook leaves flags alone");
    }

    let mut m = int15_machine(16);
    prime_dos_int_frame(&mut m);
    m.cpu.registers.set_eax(0xCAFE_1120);

    assert!(m.handle_int2f(), "AX=1120h handled");

    assert_eq!(m.cpu.registers.eax(), 0xCAFE_1120);
    assert_eq!(dos_int_flags(&m) & 1, 0, "flush hook clears CF");
}

#[test]
fn int2f_disk_handler_hook_returns_previous_vectors() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0xCAFE_1300);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x1111));
    m.cpu.registers.set_edx(0xAAAA_2222);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x3333));
    m.cpu.registers.set_ebx(0xBBBB_4444);

    assert!(m.handle_int2f(), "AH=13h first call handled");
    // The defaults are INT 13h's own per-vector stub (serviced by address
    // on every arrival route), not the legacy shared FF00:0000.
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Ds).selector,
        BIOS_ROM_IRET_SEG
    );
    assert_eq!(
        m.cpu.registers.edx(),
        0xAAAA_0000 | u32::from(bios_int_stub_off(0x13))
    );
    assert_eq!(
        m.cpu.registers.segment(SegmentIndex::Es).selector,
        BIOS_ROM_IRET_SEG
    );
    assert_eq!(
        m.cpu.registers.ebx(),
        0xBBBB_0000 | u32::from(bios_int_stub_off(0x13))
    );

    m.cpu.registers.set_eax(0xCAFE_1301);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0x5555));
    m.cpu.registers.set_edx(0xCCCC_6666);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x7777));
    m.cpu.registers.set_ebx(0xDDDD_8888);

    assert!(m.handle_int2f(), "AH=13h second call handled");
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Ds).selector, 0x1111);
    assert_eq!(m.cpu.registers.edx(), 0xCCCC_2222);
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0x3333);
    assert_eq!(m.cpu.registers.ebx(), 0xDDDD_4444);
}

#[test]
fn int2f_cdrom_reserved_debug_toggles_are_noops() {
    for ax in [0x1506u16, 0x1507] {
        let mut m = int15_machine(16);
        m.cpu.registers.set_eax(0xCAFE_0000 | u32::from(ax));
        m.cpu.registers.set_ebx(0x1111_2222);
        m.cpu.registers.set_ecx(0x3333_4444);
        m.cpu.registers.set_edx(0x5555_6666);

        assert!(m.handle_int2f(), "AX={ax:04X}h handled");

        assert_eq!(m.cpu.registers.eax(), 0xCAFE_0000 | u32::from(ax));
        assert_eq!(m.cpu.registers.ebx(), 0x1111_2222);
        assert_eq!(m.cpu.registers.ecx(), 0x3333_4444);
        assert_eq!(m.cpu.registers.edx(), 0x5555_6666);
    }
}

#[test]
fn int1a_set_and_read_date_round_trips() {
    let mut m = int15_machine(16);
    // AH=05h set date: CH/CL century/year BCD, DH/DL month/day BCD -> 2021-07-15.
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x2021);
    m.cpu.registers.set_edx(0x0715);
    m.handle_int1a();
    // AH=04h read date back.
    m.cpu.registers.set_eax(0x0400);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x2021);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0715);
}

#[test]
fn int1a_date_persists_a_non_default_century() {
    let mut m = int15_machine(16);
    // AH=05h set date to 1999-12-31 (CH=century 0x19, CL=year 0x99).
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x1999);
    m.cpu.registers.set_edx(0x1231);
    m.handle_int1a();
    // The century reached CMOS 0x32 (binary 19), not just the in-memory year.
    assert_eq!(m.rtc.century(), 19, "century persisted to CMOS 0x32");
    // AH=04h reads the full BCD date back through the century accessor.
    m.cpu.registers.set_eax(0x0400);
    m.handle_int1a();
    assert_eq!(
        m.cpu.registers.ecx() as u16,
        0x1999,
        "century and year round-trip"
    );
    assert_eq!(m.cpu.registers.edx() as u16, 0x1231);
}

#[test]
fn int1a_set_and_read_time_round_trips() {
    let mut m = int15_machine(16);
    // AH=03h set time: CH/CL hours/minutes BCD, DH seconds BCD -> 13:45:30.
    m.cpu.registers.set_eax(0x0300);
    m.cpu.registers.set_ecx(0x1345);
    m.cpu.registers.set_edx(0x3000);
    m.handle_int1a();
    m.cpu.registers.set_eax(0x0200);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1345);
    assert_eq!((m.cpu.registers.edx() as u16) >> 8, 0x30);
}

#[test]
fn int1a_day_counter_matches_calendar() {
    let mut m = int15_machine(16);
    // 1980-01-02 is day 1 since the 1980-01-01 epoch.
    m.cpu.registers.set_eax(0x0500);
    m.cpu.registers.set_ecx(0x1980);
    m.cpu.registers.set_edx(0x0102);
    m.handle_int1a();
    m.cpu.registers.set_eax(0x0A00);
    m.handle_int1a();
    assert_eq!(m.cpu.registers.ecx() as u16, 1);
}

#[test]
fn days_since_1980_handles_leap_years() {
    assert_eq!(days_since_1980(1980, 1, 1), 0);
    assert_eq!(days_since_1980(1980, 3, 1), 60); // 1980 is a leap year (31+29)
    assert_eq!(days_since_1980(1981, 1, 1), 366);
}

#[test]
fn int1a_set_day_counter_round_trips() {
    let mut m = int15_machine(16);
    // AH=0Bh latches CX into the BDA scratch word; it reads back unchanged.
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ecx(0x1234);
    m.handle_int1a();
    assert_eq!(m.memory.read_u16(BDA_DAY_COUNT).unwrap(), 0x1234);
    // CF clear: the call succeeded.
    let ss = m.cpu.registers.segment(SegmentIndex::Ss).base;
    let sp = m.cpu.registers.esp() as u16;
    let flags = m
        .memory
        .read_u16((ss + u32::from(sp.wrapping_add(4))) as usize)
        .unwrap();
    assert_eq!(flags & 0x0001, 0, "CF clear");
}

#[test]
fn int13_drive_parameters_report_real_floppy_count() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap(); // 1.44 MB
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0000); // DL=0 drive A:
    m.handle_int13();
    // One drive is mounted: DL reports 1, derived from the equipment word.
    assert_eq!(m.cpu.registers.edx() as u8, 0x01, "DL = floppy count");
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00, "AH = success");
}

#[test]
fn int13_read_over_executed_buffer_invalidates_decoded_bytes() {
    let rom = rom_with_code(&[
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xD0, // mov ss,ax
        0xBC, 0x00, 0x70, // mov sp,7000h
        0x9A, 0x00, 0x7C, 0x00, 0x00, // call far 0000:7C00
        0x31, 0xC0, // xor ax,ax
        0x8E, 0xC0, // mov es,ax
        0xBB, 0x00, 0x7C, // mov bx,7C00h
        0xB8, 0x01, 0x02, // mov ax,0201h
        0xB9, 0x01, 0x00, // mov cx,0001h
        0x31, 0xD2, // xor dx,dx
        0xCD, 0x13, // int 13h
        0xEA, 0x00, 0x7C, 0x00, 0x00, // jmp far 0000:7C00
    ]);
    let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
    machine.write_guest_block(0x7C00, &[0xB8, 0xAA, 0xAA, 0xCB]); // mov ax,AAAAh; retf

    let mut image = vec![0u8; 1_474_560];
    image[..5].copy_from_slice(&[0xFA, 0xB8, 0x34, 0x12, 0xF4]); // cli; mov ax,1234h; hlt
    machine.mount_floppy(image).unwrap();

    let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

    assert_eq!(reason, StopReason::Halted);
    assert_eq!(machine.cpu.registers.eax() as u16, 0x1234);
}

#[test]
fn int13_drive_parameters_reject_fixed_disk() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.cpu.registers.set_eax(0x0800);
    m.cpu.registers.set_edx(0x0080); // DL=0x80 fixed disk, none modeled
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x01,
        "AH = invalid drive"
    );
}

#[test]
fn int13_dasd_type_honors_drive_presence() {
    let mut m = int15_machine(16);
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    // DL=0 with a floppy mounted: AH=01 (floppy, no change line), CF clear.
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0000);
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x01,
        "AH = floppy, no change line"
    );
    // DL=1 is an absent second floppy: AH=01 and CF set.
    m.cpu.registers.set_eax(0x1500);
    m.cpu.registers.set_edx(0x0001);
    m.handle_int13();
    assert_eq!(
        (m.cpu.registers.eax() >> 8) as u8,
        0x01,
        "AH = no such drive"
    );
}

#[test]
fn int13_absent_drives_set_a_deterministic_error_for_either_incoming_carry() {
    for incoming_carry in [false, true] {
        for drive in [0x00u8, 0x80] {
            let mut m = int15_machine(16);
            prime_dos_int_frame(&mut m);
            m.memory
                .write_u16(0x9000 * 16 + 0x0104, u16::from(incoming_carry))
                .unwrap();
            m.cpu.registers.set_eax(0x0201);
            m.cpu.registers.set_edx(u32::from(drive));
            m.handle_int13();
            assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x01);
            assert_ne!(dos_int_flags(&m) & 1, 0, "drive {drive:02X} CF");
            let status = if drive >= 0x80 { 0x474 } else { 0x441 };
            assert_eq!(m.read_physical_u8(status), 0x01, "drive {drive:02X} status");
        }
    }
}

#[test]
fn bda_seeds_serial_parallel_and_video_state() {
    let m = int15_machine(16);
    // Serial/parallel base tables: COM1 + COM2 and LPT1 + LPT2 are wired.
    assert_eq!(m.memory.read_u16(0x400).unwrap(), 0x03f8); // COM1
    assert_eq!(m.memory.read_u16(0x402).unwrap(), 0x02f8); // COM2
    assert_eq!(m.memory.read_u16(0x408).unwrap(), 0x0378); // LPT1
    assert_eq!(m.memory.read_u16(0x40a).unwrap(), 0x0278); // LPT2
    // Timeout tables across all four ports each.
    assert_eq!(m.memory.read_u8(0x47f).unwrap(), 0x01); // COM4 timeout
    assert_eq!(m.memory.read_u8(0x47b).unwrap(), 0x14); // LPT4 timeout
    // Static video-state block and the system flags.
    assert_eq!(m.memory.read_u16(0x44c).unwrap(), 0x1000); // regen page size
    assert_eq!(m.memory.read_u8(0x485).unwrap(), 16); // char cell height
    assert_eq!(m.memory.read_u8(0x487).unwrap(), 0x60); // EGA/VGA video-control byte
    assert_eq!(m.memory.read_u8(0x489).unwrap(), 0x51); // EGA/VGA mode-set control
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x08); // VGA colour DCC
    assert_eq!(m.memory.read_u8(0x475).unwrap(), 0); // no fixed disks
    assert_eq!(m.memory.read_u16(0x472).unwrap(), 0x1234); // warm-boot magic
}

#[test]
fn com2_scratch_round_trips_through_the_bus() {
    // A write then read of the COM2 scratch register (0x2FF) routes through the
    // serial2 port arm exactly the way COM1's (0x3FF) does.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    bus.write_io(0x02ff, BusWidth::Byte, 0xa5, false).unwrap();
    assert_eq!(bus.read_io(0x02ff, BusWidth::Byte, 0, false).unwrap(), 0xa5);
    // COM1 stays separate: writing COM2 did not disturb COM1's scratch.
    assert_eq!(bus.read_io(0x03ff, BusWidth::Byte, 0, false).unwrap(), 0x00);
}

#[test]
fn lpt2_data_round_trips_through_the_bus() {
    // The LPT2 data latch at 0x278 reads back through the lpt2 port arm.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    bus.write_io(0x0278, BusWidth::Byte, 0x42, false).unwrap();
    assert_eq!(bus.read_io(0x0278, BusWidth::Byte, 0, false).unwrap(), 0x42);
    // The LPT2 status port reports the always-ready idle byte.
    assert_eq!(bus.read_io(0x0279, BusWidth::Byte, 0, false).unwrap(), 0xdf);
}

#[test]
fn game_port_reports_no_joystick() {
    // Port 0x201: a routine joystick probe (OUT to fire the one-shots, then
    // IN) must see the absent-joystick byte -- axis bits 0-3 clear (timers
    // already expired), button bits 4-7 set (open switches, active-low) --
    // not an UnsupportedPort fault that halts the machine.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    assert_eq!(bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    bus.write_io(0x0201, BusWidth::Byte, 0xff, false).unwrap();
    assert_eq!(bus.read_io(0x0201, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    // The ISA gameport decodes 0x200-0x207 as aliases of the one register;
    // TSUMERA probes 0x200. Both ends of the range answer, IN and OUT.
    for port in [0x0200, 0x0207] {
        bus.write_io(port, BusWidth::Byte, 0xff, false).unwrap();
        assert_eq!(bus.read_io(port, BusWidth::Byte, 0, false).unwrap(), 0xf0);
    }
}

#[test]
fn cms_probe_range_reads_open_bus_not_a_fault() {
    // Ports 0x280-0x28F are the C/MS Game Blaster's alternate probe base.
    // With no card there, a read must see open bus (0xFF) so a sound-detect
    // routine concludes "nothing present" -- not an UnsupportedPort fault
    // that halts the machine headless. Prince of Persia (PRINCE ADLIB) reads
    // 0x283 during its scan; regression guard for the passive-port entry.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    for port in [0x0280u16, 0x0283, 0x028f] {
        assert_eq!(
            bus.read_io(port, BusWidth::Byte, 0, false).unwrap(),
            0xff,
            "port {port:#06x} must read open bus"
        );
    }
    // The stub stays bounded: one past the top still faults, so genuinely
    // unclaimed ISA reads elsewhere keep surfacing as real faults.
    assert!(matches!(
        bus.read_io(0x0290, BusWidth::Byte, 0, false),
        Err(BusError::UnsupportedPort { port }) if port == 0x0290
    ));
}

#[test]
fn upper_dma_page_register_aliases_the_canonical_register() {
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    assert_eq!(bus.read_io(0x0099, BusWidth::Byte, 0, false).unwrap(), 0);
    bus.write_io(0x0099, BusWidth::Byte, 0x88, false).unwrap();
    assert_eq!(bus.read_io(0x0099, BusWidth::Byte, 0, false).unwrap(), 0x88);
    assert_eq!(bus.read_io(0x0089, BusWidth::Byte, 0, false).unwrap(), 0x88);
    bus.write_io(0x0089, BusWidth::Byte, 0x42, false).unwrap();
    assert_eq!(bus.read_io(0x0099, BusWidth::Byte, 0, false).unwrap(), 0x42);
}

#[test]
fn vmware_backdoor_probe_reads_open_bus_not_a_fault() {
    // Port 0x5658 is the VMware backdoor detection port: real VMware sets
    // EAX/EBX/ECX/EDX on `IN EAX, DX` (DX=0x5658, EAX='VMXh'); real,
    // non-VMware hardware has nothing there, so the guest must see open
    // bus (all-ones) and conclude "not VMware" -- not an UnsupportedPort
    // fault that halts the machine. JEMMEX runs this probe and used to
    // crash with CpuError("unsupported I/O port 0x5658") before this stub
    // existed; regression guard for the passive-port entry.
    let mut m = int15_machine(16);
    let mut bus = m.make_bus();
    assert_eq!(
        bus.read_io(0x5658, BusWidth::Dword, 0, false).unwrap(),
        0xffff_ffff,
        "VMware backdoor port must read open bus on a dword IN, not the VMXh response"
    );
    for port in [0x5658u16, 0x5659, 0x565a, 0x565b] {
        assert_eq!(
            bus.read_io(port, BusWidth::Byte, 0, false).unwrap(),
            0xff,
            "port {port:#06x} must read open bus"
        );
    }
    // OUT is accepted, matching every other passive stub (the generic
    // passive-port table is a plain read/write latch with no VMware
    // magic-number behavior grafted on).
    bus.write_io(0x5658, BusWidth::Dword, 0x564d_5868, false)
        .unwrap();
    // The stub stays bounded: one past the top still faults.
    assert!(matches!(
        bus.read_io(0x565c, BusWidth::Byte, 0, false),
        Err(BusError::UnsupportedPort { port }) if port == 0x565c
    ));
}

#[test]
fn int11_equipment_word_tracks_floppy_mount() {
    let mut m = int15_machine(16);
    // Mounting sets the floppy-installed bit; ejecting clears the floppy field.
    m.mount_floppy(vec![0u8; 1_474_560]).unwrap();
    m.cpu.registers.set_eax(0);
    m.handle_int11();
    assert_eq!(m.cpu.registers.eax() as u16 & 0x0001, 0x0001);
    m.eject_floppy();
    m.cpu.registers.set_eax(0);
    m.handle_int11();
    assert_eq!(m.cpu.registers.eax() as u16 & 0x00C1, 0x0000);
}

#[test]
fn int10_display_detection_tracks_color_and_mono_crtc() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x1A); // AL = function supported
    assert_eq!(m.cpu.registers.ebx() as u8, 0x08); // BL = VGA colour DCC
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x08);

    m.cpu.registers.set_eax(0x1A01);
    m.cpu.registers.set_ebx(0x000A);
    m.handle_int10();
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x0A);
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u8, 0x0A);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0003); // colour, 256 KiB
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f09); // feature bits, switch setting

    m.cpu.registers.set_eax(0x0007);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1A00);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u8, 0x07); // BL = VGA mono DCC
    assert_eq!(m.memory.read_u8(0x48A).unwrap(), 0x07);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0103); // mono, 256 KiB
}

#[test]
fn int10_1232_toggles_video_addressing() {
    let mut m = int15_machine(16);
    m.write_physical_u8(VGA_TEXT_BASE, b'T');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'T');

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().video_subsystem_enabled());
    assert!(!m.video().video_memory_enabled());

    m.write_physical_u8(VGA_TEXT_BASE, b'R');
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'R');
    {
        let mut bus = m.make_bus();
        assert_eq!(bus.read_io(0x3C3, BusWidth::Byte, 0, false).unwrap(), 1);
        assert_eq!(
            bus.read_io(0x3CC, BusWidth::Byte, 0, false).unwrap() & 0x02,
            0
        );
    }

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().video_subsystem_enabled());
    assert!(m.video().video_memory_enabled());
    assert_eq!(m.read_physical_u8(VGA_TEXT_BASE), b'T');

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0032);
    m.handle_int10();
    assert!(!m.video().video_memory_enabled());
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert!(m.video().video_memory_enabled());
}

#[test]
fn int10_1230_selects_text_scanlines_on_next_mode_set() {
    let mut m = int15_machine(16);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10); // POST default: 400 lines
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x80);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x08);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f08);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 8);
    assert_eq!(m.video().raster_height(), 262);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x00);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 14);
    assert_eq!(m.video().raster_width(), 720);

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10);
    assert_eq!(m.read_physical_u8(0x488) & 0x0F, 0x09);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 16);
    assert_eq!(m.video().raster_width(), 720);
}

#[test]
fn int10_1231_toggles_default_palette_loading_on_mode_set() {
    let mut m = int15_machine(16);
    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().set_attr_palette_reg(1, 0x2A);
    m.video_mut().write_port(0x3C6, 0x0F);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().default_palette_loading_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0x08);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [1, 2, 3]);
    assert_eq!(m.video().attr_palette_reg(1), 0x2A);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0x0F));

    m.video_mut().set_dac_entry(5, 1, 2, 3);
    m.video_mut().set_attr_palette_reg(1, 0x2A);
    m.video_mut().write_port(0x3C6, 0x0F);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().default_palette_loading_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0x00);

    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [0x2A, 0x00, 0x2A]);
    assert_eq!(m.video().attr_palette_reg(1), 1);
    assert_eq!(m.video_mut().read_port(0x3C6), Some(0xFF));
}

#[test]
fn int10_1233_toggles_grayscale_summing_for_dac_loads() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().grayscale_summing_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x02, 0x02);

    m.cpu.registers.set_eax(0x1010);
    m.cpu.registers.set_ebx(5);
    m.cpu.registers.set_edx(63 << 8); // DH = red
    m.cpu.registers.set_ecx(0); // CH/CL = green/blue
    m.handle_int10();
    assert_eq!(m.video().dac_entry(5), [18, 18, 18]);

    m.video_mut().write_port(0x3C8, 6);
    m.video_mut().write_port(0x3C9, 0);
    m.video_mut().write_port(0x3C9, 63);
    m.video_mut().write_port(0x3C9, 0);
    assert_eq!(m.video().dac_entry(6), [37, 37, 37]);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().grayscale_summing_enabled());
    assert_eq!(m.read_physical_u8(0x489) & 0x02, 0x00);

    m.cpu.registers.set_eax(0x1010);
    m.cpu.registers.set_ebx(7);
    m.cpu.registers.set_edx(0);
    m.cpu.registers.set_ecx(63 << 8);
    m.handle_int10();
    assert_eq!(m.video().dac_entry(7), [0, 63, 0]);
}

#[test]
fn int10_1234_toggles_cursor_emulation_without_disturbing_mode_set_bits() {
    let mut m = int15_machine(16);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x01);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x00);

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x11, 0x10);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x11, 0x11);
}

#[test]
fn int10_1235_acknowledges_display_switch_interface() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0035);
    m.handle_int10();

    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().display_refresh_enabled());
    assert!(m.video().video_subsystem_enabled());
}

#[test]
fn int10_01_scales_legacy_cursor_shape_when_emulation_is_enabled() {
    let mut m = int15_machine(16);
    m.write_physical_u8(0x486, 0xA5);
    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x485), 16);
    assert_eq!(m.read_physical_u16(0x485), 16);
    assert_eq!(m.read_physical_u8(0x489) & 0x01, 0x01);

    m.cpu.registers.set_eax(0x0100);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x460).unwrap(), 0x0007);
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x01);
    assert_eq!(color_crtc_reg(&mut m, 0x0B), 0x0F);

    m.cpu.registers.set_eax(0x0300);
    m.cpu.registers.set_ebx(0);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0007);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0100);
    m.cpu.registers.set_ecx(0x0007);
    m.handle_int10();
    assert_eq!(m.memory.read_u16(0x460).unwrap(), 0x0007);
    assert_eq!(color_crtc_reg(&mut m, 0x0A), 0x00);
    assert_eq!(color_crtc_reg(&mut m, 0x0B), 0x07);
}

#[test]
fn int10_1236_toggles_video_refresh() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert!(m.video_mut().planar_write_pixel(0, 0, 0x0F, false));
    let lit = m.video_mut().render_full_frame().pixels[0];
    assert_ne!(lit, 0);

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0036);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(!m.video().display_refresh_enabled());
    assert!(m.video().video_subsystem_enabled());
    assert_eq!(m.video_mut().render_full_frame().pixels[0], 0);
    assert_eq!(m.video_mut().read_status1() & 0x01, 0x01);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0036);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert!(m.video().display_refresh_enabled());
    assert_eq!(m.video_mut().render_full_frame().pixels[0], lit);
}

#[test]
fn int10_04h_reports_no_light_pen() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x04ff);
    m.cpu.registers.set_ecx(0x1234);
    m.cpu.registers.set_edx(0x5678);
    m.handle_int10();
    assert_eq!((m.cpu.registers.eax() >> 8) as u8, 0x00);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x1234);
    assert_eq!(m.cpu.registers.edx() as u16, 0x5678);
}

#[test]
fn int10_optional_adapter_extensions_report_absent() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1500);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u16, 0x0000);

    for ax in [0x7000, 0x7100] {
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0x1111);
        m.cpu.registers.set_ecx(0x2222);
        m.cpu.registers.set_edx(0x3333);
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax() as u16, 0x0000);
        assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
        assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);
        assert_eq!(m.cpu.registers.edx() as u16, 0x0000);
    }

    m.cpu.registers.set_eax(0xBF03);
    m.cpu.registers.set_ebx(0x1111);
    m.cpu.registers.set_ecx(0x2222);
    m.cpu.registers.set_edx(0x3333);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.edx() as u16, 0x0000);

    m.cpu.registers.set_eax(0xFA00);
    m.cpu.registers.set_ebx(0x1234);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);

    for ax in [0x1402, 0x4000, 0x7200, 0x8000, 0xF000, 0xFE00, 0xFF00] {
        let before_eax = 0xCAFE_0000 | ax;
        m.cpu.registers.set_eax(before_eax);
        m.cpu.registers.set_ebx(0x1111);
        m.cpu.registers.set_ecx(0x2222);
        m.cpu.registers.set_edx(0x3333);
        m.handle_int10();
        assert_eq!(m.cpu.registers.eax(), before_eax);
        assert_eq!(m.cpu.registers.ebx() as u16, 0x1111);
        assert_eq!(m.cpu.registers.ecx() as u16, 0x2222);
        assert_eq!(m.cpu.registers.edx() as u16, 0x3333);
    }
}

#[test]
fn int10_dgis_and_extended_adapter_modes_report_absent() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x6A00);
    m.cpu.registers.set_ebx(0x1111);
    m.cpu.registers.set_ecx(0x2222);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0000);
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);

    m.cpu.registers.set_eax(0x6A01);
    m.cpu.registers.set_ecx(0x3333);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0000);

    m.cpu.registers.set_eax(0x6A02);
    m.cpu
        .registers
        .set_segment(SegmentIndex::Es, SegmentRegister::real(0x1234));
    m.cpu.registers.set_edi(0xABCD_5678);
    m.handle_int10();
    assert_eq!(m.cpu.registers.segment(SegmentIndex::Es).selector, 0);
    assert_eq!(m.cpu.registers.edi() as u16, 0);

    m.cpu.registers.set_eax(0x0003);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x449), 0x03);
    for ax in [0x0070, 0x6F05] {
        m.cpu.registers.set_eax(ax);
        m.cpu.registers.set_ebx(0x0066);
        m.handle_int10();
        assert_eq!(m.read_physical_u8(0x449), 0x03);
    }
}

#[test]
fn int10_12h_reports_vga_configuration() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0010);
    m.handle_int10();
    assert_eq!(m.cpu.registers.ebx() as u16, 0x0003); // color, 256 KB VRAM
    assert_eq!(m.cpu.registers.ecx() as u16, 0x0f09); // feature bits, color switches
}

#[test]
fn int10_12h_updates_vga_policy_latches() {
    let mut m = int15_machine(16);

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x12);
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x80); // 200 scan lines

    m.cpu.registers.set_eax(0x1202);
    m.cpu.registers.set_ebx(0x0030);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x90, 0x10); // 400 scan lines

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x489) & 0x08, 0); // palette load disabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0031);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x489) & 0x08, 0); // palette load enabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0033);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x489) & 0x02, 0); // gray summing enabled

    m.cpu.registers.set_eax(0x1201);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_ne!(m.read_physical_u8(0x487) & 0x01, 0); // cursor emulation disabled

    m.cpu.registers.set_eax(0x1200);
    m.cpu.registers.set_ebx(0x0034);
    m.handle_int10();
    assert_eq!(m.read_physical_u8(0x487) & 0x01, 0); // cursor emulation enabled
}

#[test]
fn int10_1b_fills_state_block_and_signals_vga() {
    let mut m = int15_machine(16);
    m.cpu.registers.set_eax(0x0004); // CGA mode so the BDA shadows are non-VGA
    m.handle_int10();
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ebx(0x0011);
    m.handle_int10();
    m.cpu.registers.set_eax(0x0B00);
    m.cpu.registers.set_ebx(0x0101);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1A01);
    m.cpu.registers.set_ebx(0x000A);
    m.handle_int10();
    m.cpu.registers.set_eax(0x1B00); // ES:DI = 0:0 -> block at physical 0
    m.handle_int10();
    assert_eq!(m.cpu.registers.eax() as u8, 0x1B);
    assert_eq!(m.read_physical_u16(0), INT10_FUNCTIONALITY_TABLE_OFFSET);
    assert_eq!(m.read_physical_u16(2), VGA_BIOS_SEGMENT); // functionality table segment
    let table: Vec<u8> = (0..16)
        .map(|offset| {
            m.read_physical_u8(VGA_BIOS_BASE + u32::from(INT10_FUNCTIONALITY_TABLE_OFFSET) + offset)
        })
        .collect();
    assert_eq!(table.as_slice(), &INT10_STATIC_FUNCTIONALITY);
    assert_eq!(m.read_physical_u8(4), 0x04); // video mode at +4
    assert_eq!(m.read_physical_u16(0x07), 0x4000); // regen buffer/page size
    assert_eq!(m.read_physical_u16(0x09), 0x0000); // active page start
    assert_eq!(m.read_physical_u8(0x20), 0x0A); // CGA 3D8h shadow
    assert_eq!(m.read_physical_u8(0x21), 0x31); // CGA 3D9h shadow
    assert_eq!(m.read_physical_u16(0x23), 8); // bytes per character
    assert_eq!(m.read_physical_u8(0x25), 0x0A); // BDA display-combination code
    assert_eq!(m.read_physical_u16(0x27), 4); // CGA mode 04h colors
    assert_eq!(m.read_physical_u8(0x29), 1); // CGA graphics has one page
    assert_eq!(m.read_physical_u8(0x2A), 0x00); // 200 scan lines
}

#[test]
fn vga_option_rom_has_a_safe_entry_declared_size_and_checksum() {
    let mut m = int15_machine(16);
    assert_eq!(m.read_physical_u16(VGA_BIOS_BASE), 0xAA55);
    assert_eq!(m.read_physical_u8(VGA_BIOS_BASE + 2), 0x40);
    assert_eq!(m.read_physical_u8(VGA_BIOS_BASE + 3), 0xCB);
    let sum = (0..VGA_BIOS_SPAN_SIZE).fold(0u8, |sum, offset| {
        sum.wrapping_add(m.read_physical_u8(VGA_BIOS_BASE + offset))
    });
    assert_eq!(sum, 0);
    assert_eq!(m.read_physical_u16(BDA_VIDEO_SAVE_POINTER as u32), 0x0110);

    let program = [
        0x9A, 0x03, 0x00, 0x00, 0xC0, // call far C000:0003
        0xC6, 0x06, 0x00, 0x70, 0x5A, // mov byte [7000h],5Ah
        0xFA, 0xF4, // cli; hlt
    ];
    for (offset, byte) in program.iter().copied().enumerate() {
        m.write_physical_u8(0x8000 + offset as u32, byte);
    }
    m.cpu
        .registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::real(0));
    m.cpu
        .registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::real(0));
    m.cpu.registers.eip = 0x8000;
    m.cpu.registers.set_esp(0x9000);
    m.run_until_halt_or_cycles(100_000).unwrap();
    assert_eq!(
        m.read_physical_u8(0x7000),
        0x5A,
        "ROM entry returned with RETF"
    );
}

#[test]
fn int10_exposes_video_save_pointer_and_parameter_table() {
    let mut m = int15_machine(16);
    assert_eq!(
        m.read_physical_u16(BDA_VIDEO_SAVE_POINTER as u32),
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET
    );
    assert_eq!(
        m.read_physical_u16((BDA_VIDEO_SAVE_POINTER + 2) as u32),
        VGA_BIOS_SEGMENT
    );

    let save_table = VGA_BIOS_BASE + u32::from(INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET);
    assert_eq!(
        m.read_physical_u16(save_table),
        INT10_VIDEO_PARAM_TABLE_OFFSET
    );
    assert_eq!(m.read_physical_u16(save_table + 2), VGA_BIOS_SEGMENT);
    let param_table = VGA_BIOS_BASE + u32::from(INT10_VIDEO_PARAM_TABLE_OFFSET);
    let mode03 = param_table + 0x18 * INT10_VIDEO_PARAM_ENTRY_LEN as u32;
    assert_eq!(m.read_physical_u8(mode03), 80);
    assert_eq!(m.read_physical_u8(mode03 + 1), 24);
    assert_eq!(m.read_physical_u8(mode03 + 2), 16);
    let mode12 = param_table + 0x1b * INT10_VIDEO_PARAM_ENTRY_LEN as u32;
    assert_eq!(m.read_physical_u8(mode12), 80);
    assert_eq!(m.read_physical_u8(mode12 + 1), 29);
    assert_eq!(m.read_physical_u8(mode12 + 2), 16);

    m.memory.write_u16(BDA_VIDEO_SAVE_POINTER, 0).unwrap();
    m.memory.write_u16(BDA_VIDEO_SAVE_POINTER + 2, 0).unwrap();
    m.cpu.registers.set_eax(0x000D);
    m.handle_int10();
    assert_eq!(
        m.read_physical_u16(BDA_VIDEO_SAVE_POINTER as u32),
        INT10_VIDEO_SAVE_POINTER_TABLE_OFFSET
    );
    assert_eq!(
        m.read_physical_u16((BDA_VIDEO_SAVE_POINTER + 2) as u32),
        VGA_BIOS_SEGMENT
    );
}

#[test]
fn int10_1b_reports_ega_graphics_page_count() {
    let mut m = int15_machine(16);

    for (mode, pages) in [
        (0x0D, 8),
        (0x0E, 4),
        (0x0F, 2),
        (0x10, 2),
        (0x11, 1),
        (0x12, 1),
    ] {
        m.cpu.registers.set_eax(mode);
        m.handle_int10();
        m.cpu.registers.set_eax(0x1B00);
        m.handle_int10();

        assert_eq!(m.cpu.registers.eax() as u8, 0x1B);
        assert_eq!(m.read_physical_u8(0x29), pages, "mode {mode:02X}");
    }
}

#[test]
fn timeline_tracks_the_active_mode_without_reinterpreting_elapsed_time() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert_eq!(machine.active_mode(), GswMode::Gsw386);
    assert_eq!(machine.timeline.ticks_per_cpu_clock(), 300);
    machine.advance_devices_clocks(1);
    let before = machine.master_ticks();
    machine.set_mode(GswMode::Gsw586);
    assert_eq!(machine.active_mode(), GswMode::Gsw586);
    assert_eq!(machine.timeline.ticks_per_cpu_clock(), 33);
    assert_eq!(machine.master_ticks(), before);
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    assert_eq!(machine.timeline.ticks_per_cpu_clock(), 900);
    assert_eq!(machine.master_ticks(), before);
}

#[test]
fn profile_construction_and_set_mode_drive_cpu_and_cache_table() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        vec![0u8; BIOS_ROM_SIZE],
    )
    .unwrap();
    assert_eq!(machine.cpu.mode(), GswMode::Gsw386);
    assert_eq!(machine.cpu.persona(), CpuPersona::I386);
    assert_eq!(machine.cache_config(), (0, 64));

    let generation = machine.cpu.decode_cache_generation();
    machine.set_mode(GswMode::Gsw386Slow);
    assert_eq!(machine.cpu.mode(), GswMode::Gsw386Slow);
    assert_eq!(machine.cpu.persona(), CpuPersona::I386);
    assert_eq!(machine.cache_config(), (0, 64));
    assert_eq!(
        machine.cpu.decode_cache_generation(),
        generation.wrapping_add(1)
    );

    machine.set_mode(GswMode::Gsw386);
    assert_eq!(machine.cpu.mode(), GswMode::Gsw386);
    assert_eq!(machine.cpu.persona(), CpuPersona::I386);
    assert_eq!(machine.cache_config(), (0, 64));

    machine.set_mode(GswMode::Gsw486);
    assert_eq!(machine.cpu.mode(), GswMode::Gsw486);
    assert_eq!(machine.cpu.persona(), CpuPersona::I486);
    assert_eq!(machine.cache_config(), (8, 256));

    machine.set_mode(GswMode::Gsw586);
    assert_eq!(machine.cpu.mode(), GswMode::Gsw586);
    assert_eq!(machine.cpu.persona(), CpuPersona::I586);
    assert_eq!(machine.cache_config(), (32, 512));
}

#[test]
fn lotura_code_3_selects_386_slow_mode() {
    assert_eq!(GswMode::from_register_code(3), Some(GswMode::Gsw386Slow));
    assert_eq!(GswMode::Gsw386Slow.register_code(), 3);
    assert_eq!(GswMode::Gsw386Slow.persona(), CpuPersona::I386);
}
