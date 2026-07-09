// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn run_cpuid(leaf: u32) -> CpuGsw {
    // CPUID (0F A2) with the leaf selector in EAX. Returns the CPU after one step so the
    // caller can read EAX/EBX/ECX/EDX.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(leaf);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu
}

#[test]
fn cpuid_leaf0_reports_vendor_string_and_max_leaf() {
    let cpu = run_cpuid(0);
    // Max basic leaf is 1.
    assert_eq!(cpu.registers.eax(), 1);
    // Vendor string "Genuine GSW " in the standard EBX, EDX, ECX order, four bytes
    // little-endian per register.
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
    assert_eq!(cpu.registers.edx().to_le_bytes(), *b"ine ");
    assert_eq!(cpu.registers.ecx().to_le_bytes(), *b"GSW ");
    // Concatenating EBX:EDX:ECX yields the full 12-byte vendor string.
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
    vendor[4..8].copy_from_slice(&cpu.registers.edx().to_le_bytes());
    vendor[8..12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
    assert_eq!(&vendor, b"Genuine GSW ");
}

#[test]
fn cpuid_leaf1_reports_family5_and_mmx_without_fpu() {
    let cpu = run_cpuid(1);
    let eax = cpu.registers.eax();
    // Family is bits 11-8 and must be 5 (586 / K6 class).
    assert_eq!((eax >> 8) & 0xf, 5);
    // Type is bits 13-12 (OEM = 0).
    assert_eq!((eax >> 12) & 0x3, 0);
    // MMX is bit 23 of EDX; FPU is bit 0 and must be off.
    let edx = cpu.registers.edx();
    assert_ne!(edx & (1 << 23), 0, "MMX bit should be set");
    assert_eq!(edx & 1, 0, "FPU bit should be clear");
    // Brand index 0 and no extended feature claimed.
    assert_eq!(cpu.registers.ebx(), 0);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn cpuid_unknown_leaf_returns_zeros() {
    let cpu = run_cpuid(0x4000_0000);
    assert_eq!(cpu.registers.eax(), 0);
    assert_eq!(cpu.registers.ebx(), 0);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.registers.edx(), 0);
}

#[test]
fn cpuid_brand_string_reports_genuine_gsw_80586() {
    // Leaf 0x80000000 reports the maximum extended leaf, and 0x80000002..0x80000004
    // return the 48-byte null-padded brand string, 16 bytes per leaf in EAX, EBX, ECX,
    // EDX order. Concatenated, they spell the full processor name "Genuine GSW-80586".
    assert_eq!(run_cpuid(0x8000_0000).registers.eax(), 0x8000_0006);
    let mut brand = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
        .iter()
        .enumerate()
    {
        let cpu = run_cpuid(*leaf);
        let base = i * 16;
        brand[base..base + 4].copy_from_slice(&cpu.registers.eax().to_le_bytes());
        brand[base + 4..base + 8].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
        brand[base + 8..base + 12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
        brand[base + 12..base + 16].copy_from_slice(&cpu.registers.edx().to_le_bytes());
    }
    let mut expected = [0u8; 48];
    expected[..17].copy_from_slice(b"Genuine GSW-80586");
    assert_eq!(brand, expected);
}

#[test]
fn cpuid_is_not_privileged_at_cpl3() {
    // CPUID runs at any privilege level. In protected mode at CPL 3 it must execute,
    // not fault, and still report the GSW-586 identity.
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(vec![0x0f, 0xa2, 0, 0]);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.registers.eax(), 1);
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
}

#[test]
fn default_level_is_full_isa() {
    // The core resets to the full ISA so firmware POST is never restricted.
    assert_eq!(CpuGsw::default().level(), CpuLevel::I586);
}

#[test]
fn cpu_level_cache_table() {
    assert_eq!(CpuLevel::I286.cache_kb(), (0, 0));
    assert_eq!(CpuLevel::I386.cache_kb(), (0, 64));
    assert_eq!(CpuLevel::I486.cache_kb(), (16, 128));
    assert_eq!(CpuLevel::I586.cache_kb(), (32, 512));
}

// --- Phase 5 Slice A: RDTSC, RDMSR/WRMSR, the K6 MSR set, CR4 ---

#[test]
fn level_timing_scales_instruction_clocks_per_mode() {
    // 01 D8 is ADD AX, BX: a register ALU op that never faults. This measures the
    // CPU's INSTRUCTION-clock charge only (cpu.elapsed_clocks holds scaled core
    // clocks; bus/fetch clocks are accounted on the bus, not here).
    //
    // Calibration note (B-T10): the per-mode `level_timing` scalar is the COMPUTE
    // dial only. The per-mode BUS scalar (`bus_timing`, applied in the machine's
    // `scale_bus`) now carries the modes' absolute benchmark magnitude, so a fast
    // mode pulls ahead via the bus, NOT by charging fewer instruction clocks. The
    // compute dial just trims each mode's compute share to seat Dhrystone: it is
    // largest on the 286 (most compute-heavy in-order ratio), smaller on the 386,
    // and smallest-and-EQUAL on the 486 and 586 (their pull-ahead is all in the
    // bus dial). The contract this test guards: the scalar is applied per level,
    // descends 286 > 386 >= 486, and the 586 charges no more than the 486 (the bus
    // dial, not this one, carries the 586's speed). A mode change re-scales.
    fn elapsed_for(level: CpuLevel) -> u64 {
        let (mut cpu, memory) = real_mode_cpu(&[0x01, 0xd8], 0x20);
        cpu.set_level(level);
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..1000 {
            cpu.registers.eip = 0;
            cpu.cycle(&mut bus).unwrap();
        }
        cpu.elapsed_clocks
    }
    let i286 = elapsed_for(CpuLevel::I286);
    let i386 = elapsed_for(CpuLevel::I386);
    let i486 = elapsed_for(CpuLevel::I486);
    let i586 = elapsed_for(CpuLevel::I586);
    // 286 (3/5) charges more instruction clocks than the 386 (2/5).
    assert!(
        i286 > i386,
        "286 ({i286}) should charge more instruction clocks than 386 ({i386})"
    );
    // 386 (2/5) charges more than the small-and-equal 486/586 (1/12).
    assert!(
        i386 > i486,
        "386 ({i386}) should charge more instruction clocks than 486 ({i486})"
    );
    // 486 and 586 share the same compute ratio (1/12): the bus dial, not this
    // one, carries the 586's pull-ahead, so the 586 charges no MORE than the 486.
    assert!(
        i586 <= i486,
        "586 ({i586}) shares the 486's compute ratio and must charge no more than 486 ({i486})"
    );
}

#[test]
fn rdtsc_reads_elapsed_core_clocks_into_edx_eax() {
    // 0F 31. EDX:EAX take the 64-bit time-stamp counter (the running core-clock count).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x31], 0x20);
    cpu.elapsed_clocks = 0x1_0000_0002;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x0000_0002);
    assert_eq!(cpu.registers.edx(), 0x0000_0001);
}

#[test]
fn wrmsr_rdmsr_round_trip_whcr() {
    // 0F 30 WRMSR then 0F 32 RDMSR with ECX selecting WHCR. EDX:EAX is the 64-bit value.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x32], 0x20);
    cpu.registers.set_ecx(MSR_WHCR);
    cpu.registers.set_edx(0xdead_beef);
    cpu.registers.set_eax(0x0bad_f00d);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // wrmsr
    assert_eq!(cpu.msr.whcr, 0xdead_beef_0bad_f00d);
    cpu.registers.set_eax(0);
    cpu.registers.set_edx(0);
    cpu.cycle(&mut bus).unwrap(); // rdmsr
    assert_eq!(cpu.registers.edx(), 0xdead_beef);
    assert_eq!(cpu.registers.eax(), 0x0bad_f00d);
}

#[test]
fn wrmsr_efer_rejects_reserved_bits() {
    // Only EFER.SCE (bit 0) is writable; any reserved bit set raises #GP(0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_EFER);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(0x2); // bit 1 is reserved
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn wrmsr_star_rejects_reserved_bits() {
    // STAR bits 63-48 are reserved; setting one raises #GP(0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_STAR);
    cpu.registers.set_edx(0x0001_0000); // bit 48 set
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn wrmsr_efer_and_star_accept_their_defined_bits() {
    // SCE in EFER and the selector base / target EIP in STAR write without faulting.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_EFER);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(EFER_SCE as u32);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.msr.efer, EFER_SCE);
    cpu.registers.set_ecx(MSR_STAR);
    cpu.registers.set_edx(0x0000_ffff); // selector base in 47-32
    cpu.registers.set_eax(0x0001_0000); // target EIP in 31-0
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.msr.star, 0x0000_ffff_0001_0000);
}

#[test]
fn wrmsr_tsc_rebases_so_the_counter_reads_the_written_value() {
    // Writing the TSC stores an offset such that the running core-clock count reads
    // back as the written value. execute_instruction does not advance elapsed_clocks,
    // so the read is exact.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.elapsed_clocks = 500;
    cpu.registers.set_ecx(MSR_TSC);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(1_000_000);
    let mut bus = TestBus::with_memory(memory);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.time_stamp_counter(), 1_000_000);
}

#[test]
fn wrmsr_is_general_protection_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x30]);
    cpu.registers.set_ecx(MSR_WHCR);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdmsr_unknown_selector_is_general_protection() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x32], 0x20);
    cpu.registers.set_ecx(0x1234_5678);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdtsc_is_general_protection_when_tsd_set_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
    cpu.control.cr4 |= CR4_TSD;
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdtsc_runs_at_cpl3_when_tsd_clear() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
    cpu.elapsed_clocks = 42;
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.registers.eax(), 42);
}

#[test]
fn mov_cr4_round_trips() {
    // 0F 22 E0 = MOV CR4, EAX (reg=4, rm=EAX); 0F 20 E3 = MOV EBX, CR4 (reg=4, rm=EBX).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0, 0x0f, 0x20, 0xe3], 0x20);
    cpu.registers.set_eax(CR4_TSD);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr4, CR4_TSD);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), CR4_TSD);
}

#[test]
fn mov_cr_write_faults_at_cpl3() {
    // 0F 22 C0 = MOV CR0, EAX (reg=0, rm=EAX). A ring-3 write to CR0 must
    // never silently succeed -- it is a privileged instruction like every
    // other 0F 00/01 system-register op (LLDT/LTR/LMSW/CLTS all gate on
    // require_cpl0). Mirrors the cpl3_code + vector-13 shape used by the
    // RDMSR/RDTSC privilege tests above.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x22, 0xc0]);
    cpu.registers.set_eax(CR0_PE);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_cr_read_faults_at_cpl3() {
    // 0F 20 C0 = MOV EAX, CR0 (reg=0, rm=EAX). The read side has the same
    // gap as the write side; a ring-3 guest must not be able to probe CR0
    // either.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x20, 0xc0]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

// --- Batch F: LGDT/LIDT privilege, MOV CR0 PG/PE, undefined CR#, CR4 mask, CR3 PWT/PCD ---

#[test]
fn lgdt_faults_at_cpl3() {
    // 0F 01 16 xx xx = LGDT [disp16]. 386 PRM 5.1: LGDT is privileged like every other
    // 0F 00/01 system-register op, so a ring-3 guest must get #GP(0), not a silent
    // table reload.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x01, 0x16, 0x40, 0x00]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn lidt_faults_at_cpl3() {
    // 0F 01 1E xx xx = LIDT [disp16]. Same gate as LGDT above.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x01, 0x1e, 0x40, 0x00]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn lgdt_lidt_run_at_cpl0_in_real_mode() {
    // Real mode has no protection, so CPL is always 0 there; the new require_cpl0 gate
    // on LGDT/LIDT must not regress the existing 286-boot-code path (mirrors
    // lgdt_still_runs_at_286 above, but exercised here as the CPL0-unaffected half of
    // the row-22 contract).
    let mut memory = vec![0; 1024];
    // LGDT [0x0020] (5 bytes: opcode+modrm+disp16); LIDT [0x0026] starts right after.
    memory[0..5].copy_from_slice(&[0x0f, 0x01, 0x16, 0x20, 0x00]);
    memory[5..10].copy_from_slice(&[0x0f, 0x01, 0x1e, 0x26, 0x00]);
    memory[0x20..0x26].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    memory[0x26..0x2c].copy_from_slice(&[0xff, 0x01, 0x00, 0x20, 0x00, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.gdtr.base, 0x0000_1000);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.idtr.base, 0x0000_2000);
}

#[test]
fn mov_cr0_setting_pg_without_pe_is_general_protection() {
    // 0F 22 C0 = MOV CR0, EAX. 386 PRM 5.2.1: PG (bit 31) with PE (bit 0) clear is an
    // invalid combination -- paging requires protection. Run at CPL 0 (real mode) so
    // the fault is specifically the PG/PE check, not the row-23 privilege gate.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xc0], 0x20);
    cpu.registers.set_eax(CR0_PG);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
    // The rejected write must not have taken effect.
    assert_eq!(cpu.control.cr0 & CR0_PG, 0);
}

#[test]
fn mov_cr0_setting_pg_with_pe_succeeds() {
    // The companion case: PG with PE both set in the same write is the normal way
    // protected-mode paging turns on and must not fault.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xc0], 0x20);
    cpu.registers.set_eax(CR0_PE | CR0_PG);
    let mut bus = TestBus::with_memory(memory);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.control.cr0 & (CR0_PE | CR0_PG), CR0_PE | CR0_PG);
}

#[test]
fn mov_from_undefined_cr_is_undefined_opcode() {
    // 0F 20 C8 = MOV EAX, CR1 (reg=1, rm=EAX). CR1/CR5/CR6/CR7 have no backing
    // register on the 386/486/586 architecture; referencing one is #UD, not a
    // silent read of 0.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x20, 0xc8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_to_undefined_cr_is_undefined_opcode() {
    // 0F 22 F8 = MOV CR7, EAX (reg=7, rm=EAX). Same undefined-register contract as
    // the read side, checked on the write path.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xf8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_cr4_accepts_defined_bits() {
    // 0F 22 E0 = MOV CR4, EAX; 0F 20 E3 = MOV EBX, CR4. Only the bits this GSW-586
    // persona defines (VME/PVI/TSD/DE/PSE/MCE/GPE, CR4_DEFINED_MASK) exist; writing
    // exactly that set round-trips.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0, 0x0f, 0x20, 0xe3], 0x20);
    cpu.registers.set_eax(CR4_DEFINED_MASK);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr4, CR4_DEFINED_MASK);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), CR4_DEFINED_MASK);
}

#[test]
fn mov_cr4_rejects_reserved_bits() {
    // 0F 22 E0 = MOV CR4, EAX. The AMD-K6 guide's MOV-to/from-CR4 exception table and
    // the Pentium Vol. 3 instruction reference both fault ("#GP(0)"/"Interrupt 13")
    // if a 1 is written to any reserved bit -- including one way outside the defined
    // byte, like bit 31 -- in every mode, not just protected mode. CR4 is left
    // unmodified (the same convention as CR0's PG/PE gate above).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0], 0x20);
    cpu.registers.set_eax(0xffff_ffff);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
    assert_eq!(cpu.control.cr4, 0);
}

// --- Ledger row 25: MOV to/from debug registers (0F 21/0F 23) ---

#[test]
fn mov_dr7_round_trips() {
    // 0F 23 F8 = MOV DR7, EAX (reg=7, rm=EAX); 0F 21 FB = MOV EBX, DR7 (reg=7, rm=EBX).
    // Bit 10 is hardwired to 1 (DR7_FIXED_ONE) per 386 PRM ch12, so it must read back
    // set even though the write below does not include it.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xf8, 0x0f, 0x21, 0xfb], 0x20);
    cpu.registers.set_eax(0x0000_0155); // L0/G0/L1/G1/L2 enables, bit 10 not set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr7, 0x0000_0155 | DR7_FIXED_ONE);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_0155 | DR7_FIXED_ONE);
}

#[test]
fn mov_dr6_round_trips_with_reserved_bit_behavior() {
    // 0F 23 F0 = MOV DR6, EAX (reg=6, rm=EAX); 0F 21 F3 = MOV EBX, DR6 (reg=6, rm=EBX).
    // DR6 is plain storage here (breakpoint matching is ledger row 26, deferred), so
    // whatever is written reads back byte-for-byte, including into the high bits the
    // PRM defines as fixed-1 on reset -- this core does not re-force them on write.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xf0, 0x0f, 0x21, 0xf3], 0x20);
    cpu.registers.set_eax(0x0000_000f); // B0-B3 all set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr6, 0x0000_000f);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_000f);
}

#[test]
fn mov_dr6_reset_value_matches_prm() {
    // 386 PRM ch12: DR6 powers up as 0xFFFF_0FF0.
    let cpu = CpuGsw::default();
    assert_eq!(cpu.control.dr6, 0xffff_0ff0);
}

#[test]
fn mov_dr7_reset_value_matches_prm() {
    // 386 PRM ch12: DR7 powers up as 0x0000_0400 (bit 10 set, everything else clear).
    let cpu = CpuGsw::default();
    assert_eq!(cpu.control.dr7, 0x0000_0400);
}

#[test]
fn mov_dr4_aliases_dr6() {
    // 0F 23 E0 = MOV DR4, EAX (reg=4); 0F 21 E3 = MOV EBX, DR4. With CR4.DE clear (the
    // default -- never behaviorally set by this core), DR4 aliases DR6.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xe0, 0x0f, 0x21, 0xe3], 0x20);
    cpu.registers.set_eax(0x0000_000a);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr6, 0x0000_000a);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_000a);
}

#[test]
fn mov_dr5_aliases_dr7() {
    // 0F 23 E8 = MOV DR5, EAX (reg=5); 0F 21 EB = MOV EBX, DR5. Aliases DR7, same as
    // DR4 aliases DR6 above.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xe8, 0x0f, 0x21, 0xeb], 0x20);
    cpu.registers.set_eax(0x0000_0001);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr7, 0x0000_0001 | DR7_FIXED_ONE);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_0001 | DR7_FIXED_ONE);
}

#[test]
fn mov_dr0_3_round_trip() {
    // 0F 23 D8 = MOV DR3, EAX (reg=3); 0F 21 DB = MOV EBX, DR3. Linear breakpoint
    // address storage only, no matching implemented.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xd8, 0x0f, 0x21, 0xdb], 0x20);
    cpu.registers.set_eax(0xdead_beef);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr0_3[3], 0xdead_beef);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0xdead_beef);
}

#[test]
fn mov_dr_write_faults_at_cpl3() {
    // 0F 23 F8 = MOV DR7, EAX. Debug-register access is privileged (386 PRM ch12):
    // a ring-3 guest must get #GP(0), not a silent write.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x23, 0xf8]);
    cpu.registers.set_eax(0);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_dr_read_faults_at_cpl3() {
    // 0F 21 F8 = MOV EAX, DR7. Same gate on the read side.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x21, 0xf8]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_dr_memory_operand_is_undefined_opcode() {
    // 0F 21 00 = MOV [BX+SI], DR0 with mode=0 (memory operand) instead of mode=3
    // (register). Debug-register moves are register-form only; any other ModRM mode
    // is an invalid encoding (#UD), same convention as MOV CR.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x21, 0x00], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_cr3_round_trips_pwt_and_pcd() {
    // 0F 22 D8 = MOV CR3, EAX (reg=3, rm=EAX); 0F 20 DB = MOV EBX, CR3 (reg=3, rm=EBX).
    // 386 PRM 5.2.2 defines the page-directory base in bits 31:12; PWT/PCD (bits 4:3)
    // are a 486+ addition (Pentium Vol. 3 S9/S18.3) this persona implements as
    // cache-control hints. A guest that sets PWT/PCD must read them back; only bits
    // 2:0 (always reserved/0) and the base-alignment bits outside 31:12 are not
    // preserved by the base itself.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xd8, 0x0f, 0x20, 0xdb], 0x20);
    // Base 0x00123000 with PWT (bit 3) and PCD (bit 4) both set, plus reserved bits
    // 2:0 set to confirm those stay masked off.
    cpu.registers.set_eax(0x0012_3007 | 0x18);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr3, 0x0012_3018);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0012_3018);
}

#[test]
fn cpuid_leaf1_reports_tsc_and_msr() {
    let edx = run_cpuid(1).registers.edx();
    assert_ne!(edx & (1 << 4), 0, "TSC feature bit should be set");
    assert_ne!(edx & (1 << 5), 0, "MSR feature bit should be set");
}

#[test]
fn rdtsc_is_undefined_opcode_below_586() {
    // RDTSC is a 586 addition: #UD at the throttled 486 level, fine at 586.
    let code = [0x0f, 0x31];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

// --- Phase 5 Slice B: CMOVcc, FCMOVcc, FCOMI/FUCOMI ---

#[test]
fn cmove_word_moves_low_half_when_zf_set() {
    // 0F 44 C3: CMOVE AX, BX (16-bit operand in real mode).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    // A 16-bit move writes only the low word; the upper half of EAX is preserved.
    assert_eq!(cpu.registers.eax(), 0x1111_bbbb);
}

#[test]
fn cmove_does_not_move_when_zf_clear() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x1111_1111);
}

#[test]
fn cmovne_dword_moves_when_zf_clear() {
    // 66 0F 45 C3: CMOVNE EAX, EBX (32-bit operand).
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x0f, 0x45, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
}

#[test]
fn cmovcc_is_undefined_opcode_below_586() {
    let code = [0x0f, 0x44, 0xc3];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn fcmove_moves_st1_into_st0_when_zf_set() {
    // DA C9: FCMOVE ST(0), ST(1).
    let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
    cpu.fpu.push(2.0); // ST(1)
    cpu.fpu.push(1.0); // ST(0)
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 2.0);
}

#[test]
fn fcmove_leaves_st0_when_zf_clear() {
    let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
    cpu.fpu.push(2.0);
    cpu.fpu.push(1.0);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fcmovnb_moves_st1_into_st0_when_cf_clear() {
    // DB C1: FCMOVNB ST(0), ST(1).
    let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xc1], 0x20);
    cpu.fpu.push(7.0); // ST(1)
    cpu.fpu.push(3.0); // ST(0)
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 7.0);
}

#[test]
fn fcomi_sets_integer_flags_from_the_comparison() {
    // DB F1: FCOMI ST(0), ST(1). The result lands in ZF/PF/CF.
    fn run(st0: f64, st1: f64) -> (bool, bool, bool) {
        let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xf1], 0x40);
        cpu.fpu.push(st1);
        cpu.fpu.push(st0);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        (cpu.flag(FLAG_ZF), cpu.flag(FLAG_PF), cpu.flag(FLAG_CF))
    }
    assert_eq!(run(2.0, 1.0), (false, false, false)); // ST0 > ST1
    assert_eq!(run(1.0, 2.0), (false, false, true)); // ST0 < ST1
    assert_eq!(run(1.0, 1.0), (true, false, false)); // equal
    assert_eq!(run(f64::NAN, 1.0), (true, true, true)); // unordered
}

#[test]
fn fcomip_compares_then_pops() {
    // DF F1: FCOMIP ST(0), ST(1). Equal operands set ZF, then ST(0) is popped.
    let (mut cpu, memory) = real_mode_cpu(&[0xdf, 0xf1], 0x40);
    cpu.fpu.push(2.0);
    cpu.fpu.push(2.0);
    let top_before = cpu.fpu.top();
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.fpu.top(), (top_before + 1) & 7);
}

// --- Phase 5 Slice C: CMPXCHG8B ---

fn read_dword(memory: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([memory[at], memory[at + 1], memory[at + 2], memory[at + 3]])
}

#[test]
fn cmpxchg8b_equal_stores_ecx_ebx_and_sets_zf() {
    // 0F C7 0E 40 00: CMPXCHG8B [0x0040] (reg=/1, mod=0 rm=6 direct disp16).
    let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x44].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    memory[0x44..0x48].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    cpu.registers.set_eax(0x5566_7788); // EDX:EAX equals the memory value
    cpu.registers.set_edx(0x1122_3344);
    cpu.registers.set_ebx(0xcafe_babe); // ECX:EBX is the value to store
    cpu.registers.set_ecx(0xdead_beef);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(read_dword(&bus.memory, 0x40), 0xcafe_babe);
    assert_eq!(read_dword(&bus.memory, 0x44), 0xdead_beef);
}

#[test]
fn cmpxchg8b_unequal_loads_edx_eax_and_clears_zf() {
    let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x44].copy_from_slice(&0xaaaa_bbbbu32.to_le_bytes());
    memory[0x44..0x48].copy_from_slice(&0xcccc_ddddu32.to_le_bytes());
    cpu.registers.set_eax(0x0000_0001); // EDX:EAX differs from memory
    cpu.registers.set_edx(0x0000_0002);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
    assert_eq!(cpu.registers.edx(), 0xcccc_dddd);
    assert_eq!(read_dword(&bus.memory, 0x40), 0xaaaa_bbbb); // memory unchanged
}

#[test]
fn cmpxchg8b_register_form_is_undefined_opcode() {
    // 0F C7 C9: mod=3 register form is #UD. CMPXCHG8B is converted (`DecodeGroup::Misc`), so
    // drive it through the split — the executor re-detects the register form and #UDs.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn cmpxchg8b_wrong_group_extension_is_undefined_opcode() {
    // 0F C7 06 40 00: reg=/0, not CMPXCHG8B -> #UD. Driven through the split (converted).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0x06, 0x40, 0x00], 0x80);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn lock_cmpxchg8b_to_memory_is_accepted() {
    // F0 0F C7 0E 40 00: LOCK CMPXCHG8B [0x0040].
    let (mut cpu, mut memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
    cpu.registers.set_ebx(0x11);
    cpu.registers.set_ecx(0x22);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF)); // EDX:EAX = 0 equals zeroed memory
    assert_eq!(read_dword(&bus.memory, 0x40), 0x11);
    assert_eq!(read_dword(&bus.memory, 0x44), 0x22);
}

#[test]
fn lock_cmpxchg8b_register_form_is_undefined_opcode() {
    // F0 0F C7 C9: LOCK on the register form -> #UD.
    let (mut cpu, memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn cpuid_leaf1_reports_cx8() {
    let edx = run_cpuid(1).registers.edx();
    assert_ne!(edx & (1 << 8), 0, "CX8 feature bit should be set");
}

#[test]
fn cmpxchg8b_is_undefined_opcode_below_586() {
    let code = [0x0f, 0xc7, 0x0e, 0x40, 0x00];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

// --- Phase 5 Slice D: SYSCALL/SYSRET and RSM ---

#[test]
fn syscall_jumps_to_star_target_and_loads_flat_segments() {
    // 0F 05 SYSCALL. STAR: target EIP = 0x0001_0000, CS/SS selector base = 0x08.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x40);
    cpu.msr.efer = EFER_SCE;
    cpu.msr.star = (0x0008u64 << 32) | 0x0001_0000;
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ecx(), 2); // return address (EIP past the 2-byte SYSCALL)
    assert_eq!(cpu.registers.eip, 0x0001_0000);
    let cs = cpu.registers.cs();
    assert_eq!(cs.selector, 0x08);
    assert_eq!(cs.base, 0);
    assert_eq!(cs.limit, 0xffff_ffff);
    assert!(cs.default_size_32);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x10); // base + 8
    assert!(!cpu.flag(FLAG_IF)); // SYSCALL clears IF
}

#[test]
fn syscall_is_undefined_opcode_without_sce() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x20);
    cpu.msr.efer = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn sysret_returns_to_ecx_with_cpl3_and_sets_if() {
    // 0F 07 SYSRET. STAR CS/SS base = 0x08; SYSRET forces RPL 3 and SS = base + 16.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
    cpu.msr.efer = EFER_SCE;
    cpu.msr.star = 0x0008u64 << 32;
    cpu.registers.set_ecx(0x0002_0000);
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0002_0000);
    assert_eq!(cpu.registers.cs().selector, 0x0b); // 0x08 | 3
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x1b); // (0x08 + 16) | 3
    assert!(cpu.flag(FLAG_IF)); // SYSRET sets IF
}

#[test]
fn sysret_is_general_protection_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x07]);
    cpu.msr.efer = EFER_SCE;
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn sysret_is_undefined_opcode_without_sce() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
    cpu.msr.efer = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn rsm_is_undefined_opcode_outside_smm() {
    // No SMM is modeled, so RSM always faults #UD.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xaa], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn syscall_is_undefined_opcode_below_586() {
    // Even with SCE enabled, a throttled 486-level guest sees #UD from the 586 gate.
    let mut memory = vec![0u8; 64];
    memory[..2].copy_from_slice(&[0x0f, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.set_level(CpuLevel::I486);
    cpu.msr.efer = EFER_SCE;
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn decoded_insn_stays_dense() {
    // The decode cache stores one DecodedInsn per line. At the chosen 2048 lines this caps the
    // footprint near 96 KB (see DECODE_CACHE_LINES), so the guard is against unbounded growth,
    // not a hard 32-byte target: if a field pushes it past 48 bytes, move a rarely-used field
    // behind recompute-at-execute (or shrink the cache) rather than letting the line balloon.
    assert!(
        std::mem::size_of::<DecodedInsn>() <= 48,
        "DecodedInsn grew to {} bytes",
        std::mem::size_of::<DecodedInsn>()
    );
}

#[test]
fn decode_cache_hits_only_on_matching_tag_and_generation() {
    // A real decoded instruction to store. ADD AX, BX (01 D8).
    let (mut cpu, mem) = real_mode_cpu(&[0x01, 0xd8], 0x20);
    let mut bus = TestBus::with_memory(mem);
    cpu.registers.eip = 0;
    let insn = cpu.decode(&mut bus).unwrap();

    let mut cache = DecodeCache::new(4); // mask = 3
    let lin = 0x100;
    assert!(cache.get(lin, false).is_none(), "an empty line misses");
    cache.put(lin, insn, false, lin);
    assert!(cache.get(lin, false).is_some(), "a filled line hits");
    // Same line, queried under the other D bit: must miss (a 16-bit decode must never be
    // replayed in a 32-bit code segment; the D bit is part of the hit condition).
    assert!(
        cache.get(lin, true).is_none(),
        "a D-bit mismatch on a filled line misses"
    );
    // lin + 4 lands in the same direct-mapped slot (mask 3) but carries a different tag.
    assert!(
        cache.get(lin + 4, false).is_none(),
        "a different tag in the same slot misses (no false hit)"
    );
    cache.invalidate();
    assert!(
        cache.get(lin, false).is_none(),
        "a generation bump invalidates every stamped line"
    );
    cache.put(lin, insn, false, lin);
    assert!(
        cache.get(lin, false).is_some(),
        "re-filling after a bump hits again"
    );
}

#[test]
fn decode_cache_invalidate_skips_zero_on_wrap() {
    // The generation must never land back on 0 (a fresh line's default), or stale lines alias.
    let mut cache = DecodeCache::new(2);
    cache.generation = u32::MAX;
    cache.invalidate();
    assert_eq!(cache.generation, 1, "wrap skips 0");
}

#[test]
fn cycle_serves_a_repeated_instruction_from_the_decode_cache() {
    // INC AX (0x40): one byte, no branch, so re-executing at the same linear address is a hit.
    let (mut cpu, mem) = real_mode_cpu(&[0x40], 0x20);
    let mut bus = TestBus::with_memory(mem);
    let lin = cpu.linear_eip();

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "first run increments AX");
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "cycle caches the decoded instruction"
    );

    // Re-run at the same linear address: served from the cache, identical effect.
    cpu.set_eip(0);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 2, "cached INC AX runs again");

    // A CS load NEVER flushes the decode cache: the cache is linear-keyed, the D bit is in
    // the hit condition, and the fetch limit is re-checked live at each hit. This is the
    // pmode interrupt-edge / V86 monitor round-trip case that used to flush the whole cache
    // 326M times in a Doom timedemo.
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "a same-base CS reload keeps the decode cache"
    );
    cpu.load_segment_real(SegmentIndex::Cs, 0x100);
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "a changed-base CS load keeps the line too - the linear key still identifies it"
    );
}

#[test]
fn lock_prefixed_instructions_are_not_cached() {
    // LOCK ADD [BX], AL (F0 00 07). `decode` runs check_lock_target, which peeks the lock
    // target over the bus (charging clocks that are not part of `len`) and would #UD a
    // non-lockable target. A cached replay skips both, so a LOCK instruction must re-decode
    // every time and is never cached.
    let (mut cpu, mem) = real_mode_cpu(&[0xf0, 0x00, 0x07], 0x40);
    let mut bus = TestBus::with_memory(mem);
    cpu.registers.set_eax(1); // AL = 1
    cpu.registers.set_ebx(0x20);
    let lin = cpu.linear_eip();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(bus.memory[0x20], 1, "LOCK ADD [BX], AL executed");
    assert!(
        cpu.decode_cache.get(lin, false).is_none(),
        "a LOCK-prefixed instruction must not be cached (it re-charges + re-validates each run)"
    );
}

#[test]
fn cross_page_write_into_cached_code_invalidates_it() {
    // INC AX (0x40) at page 1; a store program at page 2 overwrites that byte with 0x48 (DEC
    // AX). Executing on a different page than the write is the cross-page SMC case begin_
    // instruction's current-page check cannot catch. The store program sits at 0x2008 so none
    // of its bytes collide with 0x1000's direct-mapped slot (slot 0); a collision would evict
    // the line and mask whether SMC actually invalidated it.
    let mut memory = vec![0u8; 0x3000];
    memory[0x1000] = 0x40; // INC AX
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x48; //   = 0x48 (DEC AX opcode)
    memory[0x200a] = 0xa2; // MOV moffs16, AL
    memory[0x200b] = 0x00;
    memory[0x200c] = 0x10; //   moffs = 0x1000
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    let mut bus = TestBus::with_memory(memory);

    // 1. Run INC AX at 0x1000: caches it and marks physical page 1 as code.
    cpu.registers.set_eax(0);
    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran");
    assert!(
        cpu.decode_cache.get(0x1000, false).is_some(),
        "0x1000 is cached"
    );

    // 2. From page 2, store 0x48 over the byte at 0x1000 (a write into the cached code page).
    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x48
    cpu.cycle(&mut bus).unwrap(); // MOV [0x1000], AL -> record_write_page bumps the generation
    assert!(
        cpu.decode_cache.get(0x1000, false).is_none(),
        "a write into the cached code page invalidated it"
    );

    // 3. Re-run at 0x1000: re-decodes the NEW opcode 0x48 (DEC AX), not the stale INC AX.
    cpu.registers.set_eax(5);
    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        4,
        "the freshly written DEC AX ran, not the stale cached INC AX"
    );
}

#[test]
fn data_write_to_a_non_code_page_does_not_flush_the_cache() {
    // The whole point of the code-page bitmap: a plain data write must NOT invalidate the cache,
    // or a write-heavy loop (dhrystone) would re-decode every iteration. Cache code on page 1,
    // run the store program on page 2 (at 0x2008 so it does not collide with 0x1000's slot),
    // write to page 3 (never executed), assert the line lives.
    let mut memory = vec![0u8; 0x4000];
    memory[0x1000] = 0x40; // INC AX at page 1
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x99;
    memory[0x200a] = 0xa2; // MOV moffs16, AL
    memory[0x200b] = 0x50;
    memory[0x200c] = 0x30; //   moffs = 0x3050 (page 3, holds no code)
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap(); // cache INC AX, mark page 1
    assert!(cpu.decode_cache.get(0x1000, false).is_some());

    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x99
    cpu.cycle(&mut bus).unwrap(); // MOV [0x3050], AL -> page 3 is not a code page
    assert!(
        cpu.decode_cache.get(0x1000, false).is_some(),
        "a data write to a non-code page must not flush the decode cache"
    );
}

#[test]
fn a_cached_line_is_not_served_past_a_shrunken_cs_limit() {
    // A CS load no longer flushes the decode cache, so the fetch limit must be re-checked
    // live at every hit: cache INC AX at eip 0x10 under a 64 KB CS, reload CS with an
    // identical base/D but a limit BELOW 0x10, and re-enter. The line is still in the cache
    // (no flush) but must MISS to decode, which raises #GP on the out-of-limit fetch -- the
    // stale INC AX must never run.
    let mut memory = vec![0u8; 0x40];
    memory[0x10] = 0x40; // INC AX
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.set_eip(0x10);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran and was cached");
    assert!(cpu.decode_cache.get(0x10, false).is_some());

    // Same base and D, limit 0xF: eip 0x10 is now past the segment end.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            limit: 0xf,
            ..cpu.registers.cs()
        },
    );
    cpu.invalidate_code_caches_for_cs_load();
    assert!(
        cpu.decode_cache.get(0x10, false).is_some(),
        "the line itself survives the CS load (no flush)"
    );
    cpu.registers.set_eax(5);
    cpu.set_eip(0x10);
    let _ = cpu.cycle(&mut bus); // #GP on the fetch (delivery may error: no IDT set up)
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        5,
        "the out-of-limit fetch faulted; the stale cached INC AX did NOT run"
    );
}

#[test]
fn isa_gate_exempt_decodes_are_never_cached() {
    // The firmware-ROM/ring-0 ISA-gate exemptions are context, not bytes. With CS loads no
    // longer flushing the decode cache, an exempt decode entering the cache could replay at
    // CPL 3 where the same bytes must #UD -- so both exemption channels must mark the
    // decode no-cache. Ring-0 protected mode at level I286, both channels:
    //   - 0F A2 CPUID: gated by is_386plus_two_byte at I286, passes via is_ring0_protected.
    //   - 66 40 (INC EAX): the 66 prefix #UDs at pre-386 outside the exemption.
    let mut memory = vec![0u8; 256];
    memory[..2].copy_from_slice(&[0x0f, 0xa2]); // CPUID at linear 0
    memory[0x10..0x12].copy_from_slice(&[0x66, 0x40]); // INC EAX at linear 0x10
    let mut cpu = CpuGsw::default();
    cpu.set_level(CpuLevel::I286);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false, // 16-bit segment: the 66 prefix is a real override
        },
    );
    let mut bus = TestBus::with_memory(memory);

    cpu.registers.eip = 0;
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.decode_cache.get(0, false).is_none(),
        "an exempt two-byte decode (CPUID at I286, ring 0) must not be cached"
    );

    cpu.set_eip(0x10);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.decode_cache.get(0x10, false).is_none(),
        "an exempt 66-prefixed decode (pre-386, ring 0) must not be cached"
    );
}

#[test]
fn smc_above_the_byte_bitmap_coverage_invalidates_via_page_marks() {
    // Stage-2 review finding 12: extended-memory code (where DOS-extender workloads live,
    // e.g. Quake's self-patching renderer) sits above SMC_BYTE_COVERAGE. The byte bitmap
    // does not reach there; the 4 KiB page marks must catch the write, or - now that CS
    // loads no longer flush - a stale line replays FOREVER. Same shape as
    // cross_page_write_into_cached_code_invalidates_it, relocated above 2 MiB.
    const HI: usize = 0x0020_1000; // 2 MiB + 4 KiB
    let mut memory = vec![0u8; 0x0020_3000];
    memory[HI] = 0x40; // INC AX above the byte coverage
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x48; //   = 0x48 (DEC AX opcode)
    memory[0x200a] = 0xa2; // MOV moffs, AL (moffs is 32-bit under the D=1 flat segment)
    memory[0x200b..0x200f].copy_from_slice(&(HI as u32).to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    // Real-mode 64 KB limits cannot reach 2 MiB; run flat (the pmode shape that matters).
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0008, 0x9b));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x0010, 0x93));
    cpu.control.cr0 |= CR0_PE;
    let mut bus = TestBus::with_memory(memory);

    // 1. Run INC AX above 2 MiB: cached, page-marked.
    cpu.registers.set_eax(0);
    cpu.set_eip(HI as u32);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran");
    assert!(cpu.decode_cache.get(HI as u32, true).is_some(), "cached");

    // 2. Store 0x48 (DEC AX) over it from low memory.
    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x48
    cpu.cycle(&mut bus).unwrap(); // MOV [HI], AL -> page mark hits -> generation bump
    assert!(
        cpu.decode_cache.get(HI as u32, true).is_none(),
        "a write into page-marked extended-memory code invalidated the cache"
    );

    // 3. Re-run: the NEW opcode (DEC AX) executes, not the stale INC AX.
    cpu.registers.set_eax(5);
    cpu.set_eip(HI as u32);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        4,
        "the freshly written DEC AX ran, not the stale cached INC AX"
    );
}

#[test]
fn d_bit_change_at_the_same_linear_address_re_decodes() {
    // The cache is keyed on the linear address, but a decode also depends on the code segment's
    // D bit (16- vs 32-bit operand/address size). MOV (E)AX, imm (0xB8) is 3 bytes in a 16-bit
    // segment (imm16) and 5 bytes in a 32-bit one (imm32). Caching the 16-bit form and then
    // aliasing the same linear with a 32-bit code segment must re-decode, not replay the 3-byte
    // form. A real protected-mode CS load routes through invalidate_code_caches; this drives
    // that effect directly (set the 32-bit CS, then invalidate) to avoid a full GDT setup.
    let mut memory = vec![0u8; 0x100];
    memory[0..5].copy_from_slice(&[0xb8, 0x34, 0x12, 0x78, 0x56]); // B8 imm
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0); // 16-bit
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    // 16-bit: MOV AX, 0x1234 (3 bytes).
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 0x1234);
    assert_eq!(cpu.registers.eip, 3);
    assert!(cpu.decode_cache.get(0, false).is_some());

    // Alias linear 0 with a 32-bit code segment (same base 0). NO flush happens or is
    // needed: the D bit is part of the hit condition, so the cached 16-bit decode simply
    // cannot hit under the 32-bit segment.
    let cs32 = SegmentRegister {
        default_size_32: true,
        ..cpu.registers.cs()
    };
    cpu.registers.set_segment(SegmentIndex::Cs, cs32);
    assert!(
        cpu.decode_cache.get(0, false).is_some(),
        "the 16-bit line itself stays cached"
    );
    assert!(
        cpu.decode_cache.get(0, true).is_none(),
        "but it can never be served to a 32-bit code segment"
    );

    // 32-bit: MOV EAX, 0x56781234 (5 bytes), not the stale 3-byte form.
    cpu.registers.set_eax(0);
    cpu.set_eip(0);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax(),
        0x5678_1234,
        "a 32-bit immediate was read"
    );
    assert_eq!(
        cpu.registers.eip, 5,
        "the 32-bit MOV is 5 bytes, not the cached 3"
    );
}

#[test]
fn a_fetch_page_made_not_present_re_faults_after_invalidation() {
    // A cache hit must not execute an instruction whose fetch would now fault. Page linear
    // 0x1000 -> frame 0x5000 (present), cache INC AX there, then clear the PTE present bit and
    // flush (which bumps the decode generation). Re-entry must re-decode, fault on the absent
    // fetch page, and leave AX untouched -- never replay the cached INC AX. (Observed via AX
    // rather than cr2 because, with no IDT mapped, delivering the #PF cascades a second fault.)
    let mut memory = vec![0u8; 0x8000];
    memory[0x6000..0x6004].copy_from_slice(&0x0000_7007u32.to_le_bytes()); // PD[0] -> PT 0x7000
    memory[0x7004..0x7008].copy_from_slice(&0x0000_5007u32.to_le_bytes()); // PT[1] (lin 0x1000) -> 0x5000
    memory[0x5000] = 0x40; // INC AX at linear 0x1000
    let mut cpu = CpuGsw::default();
    cpu.control.cr3 = 0x6000;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0, 0x9b));
    cpu.registers.eip = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // INC AX runs (AX 0 -> 1), cached at linear 0x1000
    assert_eq!(cpu.registers.eax() & 0xffff, 1);
    assert!(cpu.decode_cache.get(0x1000, true).is_some());

    // Clear the PTE present bit and flush so the cache invalidates.
    bus.memory[0x7004..0x7008].copy_from_slice(&0x0000_5006u32.to_le_bytes());
    cpu.flush_tlb_and_code_caches();
    assert!(
        cpu.decode_cache.get(0x1000, true).is_none(),
        "the flush invalidated the cache"
    );

    cpu.registers.set_eax(5);
    cpu.set_eip(0x1000);
    let _ = cpu.cycle(&mut bus); // faults on the absent fetch page (delivery may error: no IDT)
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        5,
        "the re-fetch from the now-absent page faulted; the stale cached INC AX did NOT run"
    );
}

#[test]
fn note_a20_changed_invalidates_the_decode_cache() {
    // A20 is masked at the bus, not the CPU, so toggling it changes which physical bytes back a
    // linear address near the 1 MB wrap without any CPU-visible state change. The machine calls
    // note_a20_changed on the transition so a cached decode of the old bytes is not replayed.
    let (mut cpu, mem) = real_mode_cpu(&[0x40], 0x20);
    let mut bus = TestBus::with_memory(mem);
    let lin = cpu.linear_eip();
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.decode_cache.get(lin, false).is_some());

    cpu.note_a20_changed();
    assert!(
        cpu.decode_cache.get(lin, false).is_none(),
        "an A20 toggle invalidates the decode cache"
    );
}

fn run_at_level(code: &[u8], level: CpuLevel) -> Result<CycleOutcome, InternalFault> {
    let mut memory = vec![0; 1024];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.set_level(level);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    // Route through the production split. Prefix-gating (66/67 at I286, the 0F two-byte ISA gate)
    // lives in `decode`, so the #UD-at-286 assertions hold on the same path the guest runs.
    exec_one_split(&mut cpu, &mut bus)
}

#[test]
fn movzx_is_undefined_opcode_at_286_but_runs_at_386() {
    // 0F B6 C3: MOVZX AX, BL. A 386 addition, so #UD at the 286 level and fine above it.
    let code = [0x0f, 0xb6, 0xc3];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "MOVZX must raise #UD at I286"
    );
    assert!(
        run_at_level(&code, CpuLevel::I386).is_ok(),
        "MOVZX must execute at I386"
    );
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn firmware_rom_cs_is_exempt_from_the_286_gate() {
    // MOVZX AX, BL is a 386 op: guest code at the 286 level #UDs on it, but the
    // BIOS ROM must keep running the full ISA so a lowered GSW mode never faults
    // firmware (Accept, interrupt service, boot). CS in the F-segment ROM
    // aperture (base 0xF0000) is the exemption.
    let code = [0x0f, 0xb6, 0xc3];
    assert!(
        matches!(
            run_at_level(&code, CpuLevel::I286).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ),
        "guest MOVZX must still #UD at I286"
    );
    let mut memory = vec![0u8; 0x10_0000];
    let base = 0x000F_0000usize;
    memory[base..base + code.len()].copy_from_slice(&code);
    let mut cpu = CpuGsw::default();
    cpu.set_level(CpuLevel::I286);
    cpu.load_segment_real(SegmentIndex::Cs, 0xF000);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0042);
    let mut bus = TestBus::with_memory(memory);
    // Drive through the split (MOVZX is a converted group now); the firmware-ROM exemption to
    // the 286 ISA gate lives in `decode`, which `exec_one_split` exercises. Confirm the op truly
    // ran (AX = zero-extended BL) rather than merely not faulting.
    assert!(
        exec_one_split(&mut cpu, &mut bus).is_ok(),
        "MOVZX fetched from BIOS ROM must run even at I286"
    );
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        0x0042,
        "MOVZX must have zero-extended BL into AX"
    );
}

#[test]
fn two_byte_convention_charges_the_second_byte_exactly_once() {
    // RDTSC (0F 31) is a two-byte op routed through `DecodeGroup::Misc` (it leaf-calls
    // `execute_two_byte`). The two-byte decode convention folds the second byte into
    // `insn.opcode` as 0x0F31 in `decode`, and the executor never re-reads it. Guard that
    // single-charge here: running RDTSC through the production split must advance eip past both
    // bytes, write a sane TSC into EDX:EAX, and charge exactly 3 instruction fetches (one
    // prefetch-window peek plus the two opcode bytes 0x0F and 0x31). A second-byte double-read in
    // the convention would push the fetch count past 3; nothing else in the file pins this
    // convention property for a 0F op so directly.
    let code = [0x0f, 0x31];
    let mut mem = vec![0u8; 64];
    mem[..code.len()].copy_from_slice(&code);

    let mut split = CpuGsw::default();
    split.load_segment_real(SegmentIndex::Cs, 0);
    split.registers.eip = 0;
    split.elapsed_clocks = 42;
    let mut sbus = TestBus::with_memory(mem);
    exec_one_split(&mut split, &mut sbus).expect("RDTSC must run through the split convention");

    assert_eq!(
        split.registers.eip, 0x2,
        "eip must advance past both opcode bytes"
    );
    // RDTSC writes the running counter into EDX:EAX; with 42 clocks elapsed EAX reads it back.
    assert_eq!(split.registers.edx(), 0, "TSC high dword");
    assert_eq!(split.registers.eax(), 42, "TSC low dword = elapsed clocks");
    assert_eq!(
        seam_fetch_count(&sbus),
        3,
        "the convention must charge the second 0F byte exactly once (no re-read)"
    );
}

#[test]
fn throttled_286_raises_ud_for_an_unconverted_two_byte_opcode_via_the_new_gate() {
    // BSWAP EAX (0F C8) is a 486 addition and stays on Fallback. The ISA gate that #UDs it at
    // the 286 level now lives in `decode` (the shared convention point), not in execute_two_byte.
    // Proving an *un-converted* 0F op still #UDs confirms the gate did not get tied to the one
    // converted group.
    let code = [0x0f, 0xc8];
    assert!(
        matches!(
            run_at_level(&code, CpuLevel::I286).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ),
        "BSWAP must #UD at I286 through the new gate location"
    );
    assert!(
        run_at_level(&code, CpuLevel::I486).is_ok(),
        "BSWAP must run at I486"
    );
}

/// The single-byte opcode values that the production split does NOT hand to a real group: every
/// prefix byte `read_prefixes` consumes (the six segment overrides, the 66h/67h operand/address-
/// size prefixes, LOCK 0xF0, and REP/REPNE 0xF3/0xF2) and 0x0F (the two-byte escape), none of
/// which is an instruction on its own — `read_prefixes`/`decode` consume them before
/// `route_group` ever classifies the following opcode, so reaching them AS an opcode is a decode
/// bug — plus 0x63 (ARPL) and 0xF1 (ICEBP/INT1), which are genuinely unimplemented. Everything
/// else in the single-byte space is implemented and MUST route to a real group. This list is the
/// sole authority for "not routed as a single-byte opcode"; the coverage test below derives the
/// implemented set as its complement.
const UNIMPLEMENTED_SINGLE_BYTE: &[u8] = &[
    0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, // segment-override prefix bytes
    0x66, 0x67, // operand-size / address-size prefix bytes
    0xf0, 0xf2, 0xf3, // LOCK / REPNE / REP prefix bytes
    0x0f, // two-byte (0F) escape: folded into 0x0F00 | second by `decode`, never routed bare
    0x63, // ARPL (unimplemented)
    0xf1, // ICEBP / INT1 (unimplemented)
];

/// True when the second byte of a 0F opcode names an IMPLEMENTED two-byte instruction. The
/// complement (within 0x00..=0xff) is the un-implemented 0F space that MUST stay on
/// `TwoByteFallback` and #UD. Built from the routed sets in `route_group` plus the no-operand
/// 0F ops `execute_two_byte` still handles directly (which `route_group` sends to `Misc`).
fn implemented_two_byte(second: u8) -> bool {
    // The 0F bytes `route_group` classifies into a real group (DataMove/Branch/BitManip/
    // CondMove/SystemSeg/Misc), mirrored exactly from the 0F arm of `route_group`.
    let routed = matches!(
        second,
        // MOVZX/MOVSX (DataMove)
        0xb6 | 0xb7 | 0xbe | 0xbf
        // Jcc near (Branch)
        | 0x80..=0x8f
        // BitManip
        | 0xa3 | 0xab | 0xb3 | 0xbb | 0xba | 0xbc | 0xbd | 0xa4 | 0xa5 | 0xac | 0xad
        | 0xb0 | 0xb1 | 0xc0 | 0xc1
        // CMOVcc / SETcc / IMUL (CondMove)
        | 0x40..=0x4f | 0x90..=0x9f | 0xaf
        // SystemSeg
        | 0x00 | 0x01 | 0x02 | 0x03 | 0x06 | 0x20 | 0x21 | 0x22 | 0x23 | 0xb2 | 0xb4 | 0xb5
        // no-operand system/serializing/CPU-id + CMPXCHG8B + BSWAP + PUSH/POP FS/GS (Misc)
        | 0x05 | 0x07 | 0x08 | 0x09 | 0x30 | 0x31 | 0x32 | 0xa0 | 0xa1 | 0xa2 | 0xa8 | 0xa9
        | 0xc7 | 0xc8..=0xcf
    );
    routed || is_mmx_two_byte(second)
}

#[test]
fn every_implemented_opcode_routes_off_the_legacy_fallback() {
    // Stage-A invariant lock: after the transitional fused fallback is gone, the production
    // `decode`/`execute_decoded` seam must hand EVERY implemented opcode to a dedicated split
    // group. `DecodeGroup::Fallback`/`TwoByteFallback` are the only two variants whose executor
    // raises `Unsupported{,TwoByte}Opcode`, so proving every implemented opcode routes to some
    // OTHER variant proves production never enters the dead-end fallback for a real instruction.
    //
    // Exhaustive partition (no representative sampling): classify the entire single-byte and
    // two-byte opcode space and check the implemented/unimplemented split against the authority
    // lists above. A future edit that drops an implemented opcode back to Fallback, or adds an
    // opcode without routing it, fails here.
    let prefixes = Prefixes::default();

    for byte in 0x00u16..=0xff {
        let unimplemented = UNIMPLEMENTED_SINGLE_BYTE.contains(&(byte as u8));
        let group = CpuGsw::route_group(byte, prefixes);
        let is_fallback = matches!(group, DecodeGroup::Fallback);
        assert!(
            !matches!(group, DecodeGroup::TwoByteFallback),
            "single-byte opcode {byte:#04x} must never route to TwoByteFallback"
        );
        if unimplemented {
            assert!(
                is_fallback,
                "unimplemented single-byte opcode {byte:#04x} must stay on Fallback, got {group:?}"
            );
        } else {
            assert!(
                !is_fallback,
                "implemented single-byte opcode {byte:#04x} must route off Fallback to a real group"
            );
        }
    }

    for second in 0x00u16..=0xff {
        // `decode` folds the second byte into the opcode as 0x0F00 | second.
        let opcode = 0x0f00 | second;
        let group = CpuGsw::route_group(opcode, prefixes);
        let is_two_byte_fallback = matches!(group, DecodeGroup::TwoByteFallback);
        assert!(
            !matches!(group, DecodeGroup::Fallback),
            "two-byte opcode 0F {second:#04x} must route via the 0F map, never plain Fallback"
        );
        if implemented_two_byte(second as u8) {
            assert!(
                !is_two_byte_fallback,
                "implemented two-byte opcode 0F {second:#04x} must route off TwoByteFallback to a real group"
            );
        } else {
            assert!(
                is_two_byte_fallback,
                "unimplemented two-byte opcode 0F {second:#04x} must stay on TwoByteFallback, got {group:?}"
            );
        }
    }
}

#[test]
fn fallback_path_is_reached_only_by_unimplemented_opcodes_and_still_uds() {
    // The runtime companion to the routing-partition test: drive each genuinely-unimplemented
    // opcode through the production split (`exec_one_split` -> `decode` -> `execute_decoded`) and
    // confirm the ONLY behavior the Fallback / TwoByteFallback arms produce is the exact
    // `Unsupported{,TwoByte}Opcode` #UD the legacy fused path produced — same error variant,
    // carrying the same `cs`. This proves the fallback arms are a pure dead-end for real
    // instructions: nothing implemented can reach them, and the unimplemented ones still #UD.
    for &op in UNIMPLEMENTED_SINGLE_BYTE {
        // The eight prefix bytes are valid as prefixes; they only #UD when they are the whole
        // instruction (no following opcode), which `read_prefixes` reaches end-of-stream on at
        // I286 but treats as a real prefix at I586. To exercise the Fallback opcode arm we need a
        // byte that is an *opcode*, never a prefix: ARPL (0x63) and ICEBP (0xf1). The prefix
        // bytes are covered by the routing-partition test above and the dedicated #UD guards.
        if matches!(op, 0x63 | 0xf1) {
            let mut cpu = CpuGsw::default();
            cpu.load_segment_real(SegmentIndex::Cs, 0);
            cpu.registers.eip = 0;
            let mut bus = TestBus::with_memory(vec![op, 0, 0, 0]);
            let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
            assert!(
                matches!(
                    err,
                    InternalFault::Exception {
                        vector: 6,
                        error_code: None
                    }
                ),
                "single-byte opcode {op:#04x} must #UD, got {err:?}"
            );
        }
    }

    // A representative un-implemented 0F byte that falls through to the generic catch-all:
    // 0x0a (unmapped). It routes to TwoByteFallback and #UDs as UnsupportedTwoByteOpcode. (0F
    // B2/B4/B5 LSS/LFS/LGS and 0F 21/23 MOV reg,DR / MOV DR,reg are now implemented -- see the
    // SystemSeg coverage test below and the ledger-row-25 `mov_dr*` tests. 0F AA RSM also routes
    // to TwoByteFallback but is an EXPLICITLY handled arm in `execute_two_byte` that #UDs with
    // vector 6 because no SMM is modeled -- it is "implemented", just always invalid -- so it is
    // not part of this generic-catch-all sweep.)
    let second = 0x0au8;
    assert!(
        !implemented_two_byte(second),
        "test bug: 0F {second:#04x} is actually implemented"
    );
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0x0f, second, 0xc0, 0, 0]);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "two-byte opcode 0F {second:#04x} must #UD, got {err:?}"
    );
}

#[test]
fn single_byte_f1_is_an_undefined_opcode() {
    // 0xF1 (ICEBP / INT1) is not implemented as a single-byte opcode. It must #UD through the
    // production split exactly like ARPL (0x63): `route_group` leaves it on Fallback and the
    // Fallback arm raises UnsupportedOpcode. Dedicated guard alongside the ARPL/prefix-byte
    // #UD tests so a future edit that mis-routes 0xF1 is caught here.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0xf1, 0, 0, 0]);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "0xF1 must raise #UD, got {err:?}"
    );
}

#[test]
fn operand_size_prefix_is_undefined_opcode_at_286() {
    // 66 B8 ... a 32-bit MOV EAX, imm32 reached through the operand-size prefix. The 286
    // has no 66h prefix, so the decoder #UDs on the prefix byte; 386 and up run it.
    let code = [0x66, 0xb8, 0x78, 0x56, 0x34, 0x12];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "the 66h operand-size prefix must raise #UD at I286"
    );
    assert!(run_at_level(&code, CpuLevel::I386).is_ok());
}

#[test]
fn address_size_prefix_is_undefined_opcode_at_286() {
    // 67 prefix: a 32-bit address form. Absent on the 286, present from the 386.
    // 67 8B 00 would be MOV with a 32-bit address; the prefix alone must #UD at I286.
    let code = [0x67, 0x90];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    // At I386 the prefix decodes and the NOP after it runs.
    assert!(run_at_level(&code, CpuLevel::I386).is_ok());
}

#[test]
fn shld_and_setcc_are_undefined_opcodes_at_286() {
    // 0F A4 SHLD and 0F 90 SETO are both 386 additions.
    let shld = [0x0f, 0xa4, 0xc3, 0x04]; // SHLD BX, AX, 4
    let setcc = [0x0f, 0x90, 0xc0]; // SETO AL
    for code in [&shld[..], &setcc[..]] {
        let fault = run_at_level(code, CpuLevel::I286).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
        assert!(run_at_level(code, CpuLevel::I386).is_ok());
    }
}

#[test]
fn lgdt_still_runs_at_286() {
    // 0F 01 /2 LGDT is a 286 instruction, so it must NOT be gated at the 286 level.
    // ModRM 16 = mode 0, reg 2 (LGDT), rm 6 (direct disp16) pointing at a 6-byte pseudo-
    // descriptor in memory.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x0f, 0x01, 0x16, 0x20]); // disp16 = 0x0020
    // 6-byte GDTR image at 0x0020: limit then 32-bit base.
    memory[0x20..0x26].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.set_level(CpuLevel::I286);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    // LGDT (0F 01 /2) is converted to the decode/execute split (task A12); run it through the
    // split, not the legacy fused entry (whose 0F 01 arm is gone).
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.gdtr.limit, 0x00ff);
    assert_eq!(cpu.gdtr.base, 0x0000_1000);
}

#[test]
fn cpuid_is_undefined_opcode_below_486() {
    // CPUID is absent on the 286 and 386; it appears on the 486 and 586.
    let code = [0x0f, 0xa2];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I286).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(matches!(
        run_at_level(&code, CpuLevel::I386).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I486).is_ok());
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn cpuid_runs_in_ring0_protected_mode_below_486() {
    // The exec-time CPUID gate carries the same ring-0 protected-mode
    // exemption as the prefix and 0F-extended gates: chipset-side ring-0
    // monitor code gets the full core ISA even when the guest persona has
    // no CPUID (I286/I386). Same flat CPL-0 code segment shape as
    // `cpl3_code`, but with an RPL-0 selector. I386 is the interesting
    // level: the I286 two-byte gate does not apply, so only the CPUID
    // gate decides. (At I486/I586 `has_cpuid()` is true and the gate is
    // moot for everyone.)
    let mut memory = vec![0u8; 256];
    memory[..2].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = CpuGsw::default();
    cpu.set_level(CpuLevel::I386);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    assert!(
        exec_one_split(&mut cpu, &mut bus).is_ok(),
        "CPUID must execute in ring-0 protected mode at the I386 level"
    );

    // Guest-facing code is still gated: CPL-3 protected mode at I386 #UDs.
    let (mut cpu3, mut bus3) = cpl3_code(&[0x0f, 0xa2]);
    cpu3.set_level(CpuLevel::I386);
    let fault = exec_one_split(&mut cpu3, &mut bus3).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "CPUID must still #UD for CPL-3 guest code at the I386 level"
    );
    // (Real-mode #UD at I286/I386 is pinned by cpuid_is_undefined_opcode_below_486.)
}

#[test]
fn cpuid_extended_leaf1_reports_amd_feature_flags() {
    let cpu = run_cpuid(0x8000_0001);
    // EAX carries the processor signature: family 5.
    assert_eq!((cpu.registers.eax() >> 8) & 0xf, 5);
    let edx = cpu.registers.edx();
    // The implemented instructions sit at their AMD extended-leaf bit positions.
    assert_ne!(edx & (1 << 10), 0, "SYSCALL/SYSRET (bit 10)");
    assert_ne!(edx & (1 << 15), 0, "integer CMOVcc (bit 15)");
    assert_ne!(edx & (1 << 16), 0, "FP FCMOVcc (bit 16)");
    assert_ne!(edx & (1 << 4), 0, "TSC");
    assert_ne!(edx & (1 << 5), 0, "MSR");
    assert_ne!(edx & (1 << 8), 0, "CX8");
    assert_ne!(edx & (1 << 23), 0, "MMX");
    // Features the GSW-586 does not emulate stay clear.
    assert_eq!(edx & 1, 0, "FPU off");
    assert_eq!(edx & (1 << 7), 0, "no machine-check exception");
}

#[test]
fn cpuid_cache_leaves_report_level_sizes() {
    // The AMD-style L1 (0x80000005) and L2 (0x80000006) leaves carry the live level's
    // cache sizes in ECX: L1 KB in bits 31-24, L2 KB in bits 31-16.
    let mut cpu = run_cpuid(0x8000_0005);
    assert_eq!(cpu.registers.ecx() >> 24, 32); // I586 L1 = 32 KB (P55C: 16K I + 16K D)
    cpu = run_cpuid(0x8000_0006);
    assert_eq!(cpu.registers.ecx() >> 16, 512); // I586 L2 = 512 KB
}

#[test]
fn id_flag_toggle_detection_sequence_finds_cpuid() {
    // The standard CPUID-presence probe: read EFLAGS, flip ID (bit 21), write it back,
    // read EFLAGS again, and conclude CPUID exists if ID changed. Model that here using
    // PUSHFD/POPFD plus a software toggle of FLAG_ID, then run CPUID leaf 0 to confirm
    // the detection concludes correctly.
    let mut memory = vec![0; 1024];
    // 66 9c PUSHFD ; 66 9d POPFD to round-trip the dword image carrying ID.
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    memory[2..4].copy_from_slice(&[0x66, 0x9d]);
    // 0f a2 CPUID with EAX = 0 already loaded.
    memory[4..6].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(memory);

    // Establish ID = 0, flip it on, and confirm the flag image carries the change so a
    // detection routine would observe ID as toggleable (CPUID present).
    let before = cpu.flag(FLAG_ID);
    cpu.set_flag(FLAG_ID, !before);
    cpu.cycle(&mut bus).unwrap(); // pushfd captures ID = 1
    cpu.set_flag(FLAG_ID, before); // perturb
    cpu.cycle(&mut bus).unwrap(); // popfd restores ID = 1
    let toggled = cpu.flag(FLAG_ID);
    assert_eq!(toggled, !before, "ID flag must be toggleable");

    // Detection concluded CPUID is present; execute it and confirm the GSW-586 vendor.
    cpu.cycle(&mut bus).unwrap(); // cpuid
    assert_eq!(cpu.registers.eax(), 1);
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
}
