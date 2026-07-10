// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn movzx_byte_zero_extends_into_ax() {
    // movzx ax, bl  (0x0f 0xb6 0xc3, modrm mod=3 reg=ax rm=bl): bl=0x80 -> ax=0x0080.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb6, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80); // bl
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
}

#[test]
fn movzx_byte_zero_extends_into_eax_clearing_high_bits() {
    // 0x66 0x0f 0xb6 0xc3 (movzx eax, bl): bl=0x80, eax preset 0xffff_ffff -> eax=0x0000_0080.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb6, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xffff_ffff);
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_0080);
}

#[test]
fn movzx_word_zero_extends_into_eax() {
    // 0x66 0x0f 0xb7 0xc3 (movzx eax, bx): bx=0x8000, eax preset 0xffff_ffff -> eax=0x0000_8000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb7, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xffff_ffff);
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_8000);
}

#[test]
fn movsx_byte_sign_extends_into_ax() {
    // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x80 -> ax=0xff80.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
}

#[test]
fn movsx_byte_sign_extends_into_eax() {
    // 0x66 0x0f 0xbe 0xc3 (movsx eax, bl): bl=0x80 -> eax=0xffff_ff80.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbe, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_ff80);
}

#[test]
fn movsx_word_sign_extends_into_eax() {
    // 0x66 0x0f 0xbf 0xc3 (movsx eax, bx): bx=0x8000 -> eax=0xffff_8000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbf, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_8000);
}

#[test]
fn movsx_byte_positive_source_zero_fills() {
    // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x7f (positive) -> ax=0x007f, no sign fill.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x7f);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x007f);
}

#[test]
fn movzx_word_into_16bit_dest_preserves_high_eax() {
    // movzx ax, bx (0x0f 0xb7 0xc3, no 0x66): a word source into a 16-bit
    // destination is a plain word move; the high half of EAX is preserved by
    // write_gpr16.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb7, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xdead_0000);
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xdead_8000);
}

#[test]
fn movzx_reads_byte_from_memory_source() {
    // movzx ax, byte [0x40] (0x0f 0xb6 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
    // [ds:0x40]=0x80 -> ax=0x0080. Exercises the memory-source decode path that the
    // register-operand tests do not, since the conformance vectors are not in CI.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xb6, 0x06, 0x40, 0x00]);
    memory[0x40] = 0x80;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
}

#[test]
fn setz_sets_byte_when_zf_set() {
    // setz bl (0x0f 0x94 0xc3): ZF=1 -> bl=1. bl preset 0xff to prove it is overwritten.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0xff);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 1);
    // SETcc writes the byte without disturbing the flag it tested.
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn setz_clears_byte_when_zf_clear() {
    // setz bl (0x0f 0x94 0xc3): ZF=0 -> bl=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0xff);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 0);
}

#[test]
fn setnz_writes_memory_destination() {
    // setnz byte [0x40] (0x0f 0x95 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
    // ZF=0 -> !ZF true -> [ds:0x40]=1.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0x95, 0x06, 0x40, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 1);
}

#[test]
fn imul_0f_af_16bit_fits_clears_carry_overflow() {
    // imul bx, cx (0x0f 0xaf 0xd9, modrm mod=3 reg=bx rm=cx): 3 * 4 = 12, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.write_reg16(Reg16::Cx, 4);
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 12);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_16bit_overflow_sets_carry_overflow() {
    // imul bx, cx (0x0f 0xaf 0xd9): 0x1000 * 0x10 = 0x10000, truncates to 0, CF=OF=1.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x1000);
    cpu.write_reg16(Reg16::Cx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_32bit_fits_clears_carry_overflow() {
    // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 1000 * 1000 = 1_000_000, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(3, 1000); // ebx
    cpu.write_gpr32(1, 1000); // ecx
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 1_000_000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_32bit_overflow_sets_carry_overflow() {
    // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 0x10000 * 0x10000 = 0x1_0000_0000,
    // truncates to 0, CF=OF=1.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(3, 0x0001_0000); // ebx
    cpu.write_gpr32(1, 0x0001_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 0x0000_0000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_signed_negative_result_fits() {
    // imul bx, cx (0x0f 0xaf 0xd9): -1 * 5 = -5 (0xfffb), fits signed 16-bit, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.write_reg16(Reg16::Cx, 0x0005);
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xfffb);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_signed_overflow_differs_from_unsigned() {
    // imul bx, cx (0x0f 0xaf 0xd9): -1 * -32768 = +32768. The low half 0x8000
    // sign-extends to -32768, not +32768, so the signed result does not fit:
    // bx=0x8000, CF=OF=1. An unsigned multiply of 0xffff * 0x8000 would truncate
    // to the same 0x8000 but read as non-overflowing, so this distinguishes IMUL.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.write_reg16(Reg16::Cx, 0x8000); // -32768
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x8000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn bsf_finds_lowest_set_bit() {
    // bsf bx, cx (0x0f 0xbc 0xd9): cx=0x0140 -> lowest set bit at index 6, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0140);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 6);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsf_zero_source_sets_zf_and_leaves_dest() {
    // bsf bx, cx (0x0f 0xbc 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0xbeef).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 0xbeef);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xbeef);
}

#[test]
fn bsf_32bit_finds_low_bit() {
    // 0x66 0x0f 0xbc 0xd9 (bsf ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbc, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0x8000_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 31); // ebx
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_finds_highest_set_bit() {
    // bsr bx, cx (0x0f 0xbd 0xd9): cx=0x0140 -> highest set bit at index 8, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0140);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 8);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_32bit_finds_high_bit() {
    // 0x66 0x0f 0xbd 0xd9 (bsr ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbd, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0x8000_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 31); // ebx
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_zero_source_sets_zf_and_leaves_dest() {
    // bsr bx, cx (0x0f 0xbd 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0x1234).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
}

#[test]
fn bt_register_reads_set_bit() {
    // bt cx, bx (0x0f 0xa3 0xd9, modrm mod=3 reg=bx rm=cx): cx=0x0008 bit 3, bx=3 -> CF=1, cx unchanged.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
}

#[test]
fn bt_register_reads_clear_bit() {
    // bt cx, bx: cx=0x0008, bx=2 (bit 2 clear) -> CF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 2);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn bts_register_sets_bit_and_reads_old() {
    // bts cx, bx (0x0f 0xab 0xd9): cx=0x0000, bx=3 -> CF=0 (old bit), cx=0x0008.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xab, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
}

#[test]
fn btr_register_clears_bit_and_reads_old() {
    // btr cx, bx (0x0f 0xb3 0xd9): cx=0x0008, bx=3 -> CF=1 (old bit), cx=0x0000.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb3, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn btc_register_toggles_bit() {
    // btc cx, bx (0x0f 0xbb 0xd9): cx=0x0008, bx=3 -> CF=1 (old), cx=0x0000 (toggled off).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbb, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn bts_memory_positive_index_walks_to_next_word() {
    // bts [0x40], bx (0x0f 0xab 0x1e 0x40 0x00, modrm mod=00 reg=bx rm=110 disp16):
    // bx=17 -> block 1, bit 1 -> word at 0x42 (0x40+2). [0x42]=0 -> CF=0, [0x42]=0x0002.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xab, 0x1e, 0x40, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 17);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x42], bus.memory[0x43]]),
        0x0002
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0000
    );
}

#[test]
fn bt_memory_negative_index_walks_to_previous_word() {
    // bt [0x40], bx (0x0f 0xa3 0x1e 0x40 0x00): bx=0xffff (-1) -> block -1, bit 15 ->
    // word at 0x3e (0x40-2). [0x3e]=0x8000 -> CF=1. BT does not write.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xa3, 0x1e, 0x40, 0x00]);
    memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
        0x8000
    );
}

#[test]
fn btc_memory_negative_index_walks_and_toggles() {
    // btc [0x40], bx (0x0f 0xbb 0x1e 0x40 0x00): bx=0xffff (-1) -> word at 0x3e, bit 15.
    // [0x3e]=0x8000 (bit 15 set) -> CF=1, the bit toggles off -> [0x3e]=0x0000.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xbb, 0x1e, 0x40, 0x00]);
    memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
        0x0000
    );
}

#[test]
fn bts_32bit_register_sets_high_bit() {
    // 0x66 0x0f 0xab 0xd9 (bts ecx, ebx): ecx=0, ebx=20 -> CF=0, ecx bit 20 set.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xab, 0xd9]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0); // ecx
    cpu.write_gpr32(3, 20); // ebx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_gpr32(1), 0x0010_0000);
}

#[test]
fn bt_immediate_reads_selected_bit() {
    // bt cx, 5 (0x0f 0xba 0xe1 0x05, modrm mod=3 reg=/4 rm=cx): cx=0x0020 bit 5 -> CF=1, cx unchanged.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xe1, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0020);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0020);
}

#[test]
fn btr_immediate_clears_selected_bit() {
    // btr cx, 5 (0x0f 0xba 0xf1 0x05, modrm mod=3 reg=/6 rm=cx): cx=0x0020 -> CF=1, cx=0x0000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xf1, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0020);
    cpu.set_flag(FLAG_CF, false); // prove CF=1 comes from the old bit, not a residual
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn bts_immediate_memory_no_walk() {
    // bts [0x40], 5 (0x0f 0xba 0x2e 0x40 0x00 0x05, modrm mod=00 reg=/5 rm=110 disp16):
    // imm bit 5, accesses [0x40] directly (no walk). [0x40]=0 -> CF=0, [0x40]=0x0020.
    let mut memory = vec![0; 128];
    memory[0..6].copy_from_slice(&[0x0f, 0xba, 0x2e, 0x40, 0x00, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0020
    );
}

#[test]
fn bt_immediate_reg_below_4_delivers_ud() {
    // 0x0f 0xba 0xc1 0x05 (modrm mod=3 reg=/0 rm=cx): reg<4 is invalid -> #UD (vector 6).
    // 1024 bytes so the stack push at 0x0100 (6 bytes) and IVT at 0x18 both fit.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xc1, 0x05]);
    memory[0x18] = 0xee; // IVT[6] IP low byte -> IP 0x00ee
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn shld_imm_shifts_left_and_fills_from_source() {
    // shld ax, bx, 4 (0x0f 0xa4 0xd8 0x04, modrm mod=3 reg=bx rm=ax):
    // ax=0x1234, bx=0x5678 -> ax=0x2345, CF=1 (bit shifted out of ax bit 12).
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x04]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
    assert!(cpu.flag(FLAG_CF));
}

#[test]
fn shrd_imm_shifts_right_and_fills_from_source() {
    // shrd ax, bx, 4 (0x0f 0xac 0xd8 0x04): ax=0x1234, bx=0x5678 -> ax=0x8123, CF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x04]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_cl_uses_cl_count() {
    // shld ax, bx, cl (0x0f 0xa5 0xd8): cl=4 -> same as imm 4: ax=0x2345, CF=1.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa5, 0xd8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
    assert!(cpu.flag(FLAG_CF));
}

#[test]
fn shrd_cl_uses_cl_count() {
    // shrd ax, bx, cl (0x0f 0xad 0xd8): cl=4 -> same as shrd imm 4: ax=0x8123, CF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xad, 0xd8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_32bit_imm() {
    // 0x66 0x0f 0xa4 0xd8 0x08 (shld eax, ebx, 8): eax=0x1234_5678, ebx=0x9abc_def0
    // -> eax=0x3456_789a, CF=0.
    let mut memory = vec![0; 64];
    memory[0..5].copy_from_slice(&[0x66, 0x0f, 0xa4, 0xd8, 0x08]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x1234_5678); // eax
    cpu.write_gpr32(3, 0x9abc_def0); // ebx
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x3456_789a);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_count_one_sets_overflow_on_sign_change() {
    // shld ax, bx, 1 (0x0f 0xa4 0xd8 0x01): ax=0x4000 -> ax=0x8000, sign flips, OF=1.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    cpu.set_flag(FLAG_OF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_one_clears_overflow_without_sign_change() {
    // shld ax, bx, 1: ax=0x0001 -> ax=0x0002, sign unchanged, OF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0002);
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_zero_is_noop() {
    // shld ax, bx, 0 (0x0f 0xa4 0xd8 0x00): ax unchanged, flags unchanged.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_past_width_rotates_source() {
    // shld ax, bx, 18 (0x0f 0xa4 0xd8 0x12): count 18 > 16 is undefined per Intel; the
    // 386 leaves ax as the source rotated left by 18 mod 16 = 2. bx=0x1234 -> ax=0x48d0.
    // The destination's prior value does not matter (preset 0xffff).
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x12]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x48d0);
}

#[test]
fn shrd_count_past_width_rotates_source() {
    // shrd ax, bx, 18 (0x0f 0xac 0xd8 0x12): the 386 leaves ax as the source rotated
    // right by 2. bx=0x1234 -> ax=0x048d.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x12]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x048d);
}

#[test]
fn xchg_byte_swaps_registers() {
    // xchg al, bl (0x86 0xc3, modrm mod=3 reg=al rm=bl). al=0x12, bl=0x34 -> al=0x34, bl=0x12.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x86, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0012);
    cpu.write_reg16(Reg16::Bx, 0x0034);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x34);
    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x12);
}

#[test]
fn xchg_word_swaps_registers() {
    // xchg bx, ax (0x87 0xc3, modrm reg=ax rm=bx). ax=0x1234, bx=0x5678 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x87, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
}

#[test]
fn xchg_word_swaps_register_and_memory() {
    // xchg [0x40], ax (0x87 0x06 0x40 0x00, modrm mod=0 reg=ax rm=110 disp16).
    let mut memory = vec![0; 128];
    memory[0..4].copy_from_slice(&[0x87, 0x06, 0x40, 0x00]);
    memory[0x40] = 0xcd;
    memory[0x41] = 0xab; // word at 0x40 = 0xabcd
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xabcd);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x1234
    );
}

#[test]
fn xchg_dword_swaps_registers() {
    // 0x66 0x87 0xc3 (xchg ebx, eax). eax=0x1111_2222, ebx=0x3333_4444 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x66, 0x87, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x1111_2222);
    cpu.write_gpr32(3, 0x3333_4444);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x3333_4444);
    assert_eq!(cpu.read_gpr32(3), 0x1111_2222);
}

#[test]
fn xchg_accumulator_swaps_ax_with_reg() {
    // xchg ax, cx (0x91). ax=0x1234, cx=0x5678 -> swapped.
    let mut memory = vec![0; 64];
    memory[0] = 0x91;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Cx, 0x5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x1234);
}

#[test]
fn xchg_accumulator_dword_swaps_eax_with_reg() {
    // 0x66 0x93 (xchg eax, ebx). eax=0x0001_0002, ebx=0x0003_0004 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x93]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x0001_0002);
    cpu.write_gpr32(3, 0x0003_0004);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x0003_0004);
    assert_eq!(cpu.read_gpr32(3), 0x0001_0002);
}

#[test]
fn xchg_byte_swaps_register_and_memory_with_displacement() {
    // xchg [bx+0x10], al (0x86 0x47 0x10, modrm mod=1 reg=al rm=[bx]+disp8).
    // bx=0x20 -> address 0x30. Guards against re-decoding the ModRm, which would
    // consume a second displacement byte and advance eip past the instruction.
    let mut memory = vec![0; 128];
    memory[0..3].copy_from_slice(&[0x86, 0x47, 0x10]);
    memory[0x30] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0020);
    cpu.write_reg16(Reg16::Ax, 0x0055); // AL = 0x55
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99); // AL got the memory byte
    assert_eq!(bus.memory[0x30], 0x55); // memory got AL
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no extra fetch
}

#[test]
fn loopne_decrements_cx_and_branches_while_not_equal() {
    // loopne +5 (0xe0 0x05). cx=3, ZF=0 -> cx=2, taken: eip = 2 + 5 = 7.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn loopne_falls_through_when_zero_flag_set() {
    // loopne +5: cx=3, ZF=1 -> cx=2, not taken (LOOPNE loops while ZF=0): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn loopne_falls_through_when_count_reaches_zero() {
    // loopne +5: cx=1, ZF=0 -> cx=0, not taken (count zero): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 1);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn loope_branches_while_equal() {
    // loope +5 (0xe1 0x05): cx=3, ZF=1 -> cx=2, taken: eip = 7.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe1, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn loope_falls_through_when_zero_flag_clear() {
    // loope +5 (0xe1 0x05): cx=3, ZF=0 -> cx=2, not taken (LOOPE loops while ZF=1): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe1, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn jcxz_branches_only_when_cx_zero() {
    // jcxz +5 (0xe3 0x05): cx=0 -> taken (eip=7), no decrement.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe3, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn jcxz_falls_through_when_cx_nonzero() {
    // jcxz +5: cx=1 -> not taken: eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe3, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 1);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 1);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn jecxz_uses_ecx_with_address_override() {
    // 0x67 jecxz +5 (0x67 0xe3 0x05): ecx=0 -> taken: eip = 3 + 5 = 8.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x67, 0xe3, 0x05]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0); // ecx = 0
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 8);
    assert_eq!(cpu.registers.ecx(), 0); // JECXZ does not decrement
}

#[test]
fn xlat_reads_ds_table_indexed_by_al() {
    // xlat (0xd7): DS:0, BX=0x10, AL=0x05 -> AL = [0x15].
    let mut memory = vec![0; 64];
    memory[0] = 0xd7;
    memory[0x15] = 0xab;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Ax, 0x0005); // AL = 5, AH = 0
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xab);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00); // AH unchanged
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0010); // BX unchanged
}

#[test]
fn xlat_wraps_the_16bit_base_plus_index() {
    // xlat: BX=0xffff, AL=0x02 -> offset = (0xffff + 2) & 0xffff = 0x0001.
    let mut memory = vec![0; 64];
    memory[0] = 0xd7;
    memory[0x01] = 0xcd;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff);
    cpu.write_reg16(Reg16::Ax, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xcd);
}

#[test]
fn xlat_honours_a_segment_override() {
    // 0x26 xlat (es override). ES base = 0x0100 << 4 = 0x1000. BX=0x10, AL=0x05 -> [0x1015].
    let mut memory = vec![0; 0x2000];
    memory[0..2].copy_from_slice(&[0x26, 0xd7]);
    memory[0x1015] = 0x99;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0x0100);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99);
}

#[test]
fn daa_low_nibble_correction() {
    // daa (0x27): AL=0x7C, CF=0, AF=0 -> AL=0x82 (low nibble +6), CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x007c);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x82);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn daa_both_corrections_set_carry() {
    // daa: AL=0xAA -> +6 = 0xB0 (AF=1), then +0x60 = 0x10 (CF=1).
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x00aa);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x10);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn daa_incoming_aux_carry_triggers_correction() {
    // daa: AL=0x20 (low nibble <= 9), AF=1 -> the first correction fires on AF alone:
    // AL=0x26, CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0020);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x26);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn das_low_nibble_correction() {
    // das (0x2f): AL=0x4A, CF=0, AF=0 -> AL=0x44 (low nibble -6), CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x2f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x004a);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x44);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn das_high_correction_on_incoming_carry() {
    // das: AL=0x00, CF=1, AF=0 -> -0x60 = 0xA0, CF=1, AF=0.
    let mut memory = vec![0; 64];
    memory[0] = 0x2f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xa0);
    assert!(cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aaa_adjusts_and_carries_into_ah() {
    // aaa (0x37): AX=0x000B (AL low nibble > 9) -> AX += 0x106, AL &= 0x0f.
    // AX=0x0111 then AL=0x01 -> AX=0x0101; CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x000b);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x01);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aaa_no_adjust_clears_carry() {
    // aaa: AX=0x0005, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aas_adjusts_and_borrows_from_ah() {
    // aas (0x3f): AX=0x020B (AL low nibble > 9) -> AX -= 6, AH -= 1, AL &= 0x0f.
    // 0x020B - 6 = 0x0205, AH-1 -> 0x0105, AL=0x05; CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x020b);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aas_no_adjust_clears_carry() {
    // aas: AX=0x0204, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0204);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x04);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x02);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aaa_aux_carry_triggers_adjust() {
    // aaa: AL=0x01 (low nibble <= 9), AF=1 -> the adjust fires on AF alone.
    // AX=0x0001 + 0x106 = 0x0107, then AL &= 0x0f -> AX=0x0107; AL=0x07, AH=0x01, CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x07);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aas_aux_carry_triggers_adjust() {
    // aas: AL=0x08 (low nibble <= 9, >= 6 so no extra AH borrow), AF=1 -> the adjust
    // fires on AF alone. AX=0x0208 - 6 = 0x0202, AH-1 -> 0x0102, AL &= 0x0f -> AX=0x0102;
    // AL=0x02, AH=0x01, CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0208);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x02);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aam_splits_al_into_ah_and_al() {
    // aam (0xd4 0x0a): AL=0x4B (75) -> AH=7, AL=5. SF=0, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xd4, 0x0a]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x004b);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x07);
    assert!(!cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn aam_zero_divisor_is_divide_error() {
    // aam (0xd4 0x00): divide by zero -> #DE, delivered through the real-mode IVT.
    const ORIGIN: usize = 0x10;
    let (mut cpu, mut memory) = real_mode_cpu(&[], 0x1_0000);
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xd4, 0x00]);
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0x004b);
    let mut bus = TestBus::with_memory(memory);

    expect_de_delivered(&mut cpu, &mut bus);
}

#[test]
fn aad_folds_ah_into_al() {
    // aad (0xd5 0x0a): AX=0x0507 (AH=5, AL=7) -> AL = 7 + 5*10 = 57 = 0x39, AH=0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xd5, 0x0a]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0507);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x39);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn lock_add_to_memory_executes() {
    // lock add [0x40], ax (0xf0 0x01 0x06 0x40 0x00). mem[0x40]=0x0010, ax=0x0005 -> 0x0015.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0xf0, 0x01, 0x06, 0x40, 0x00]);
    memory[0x40] = 0x10;
    memory[0x41] = 0x00;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0015
    );
}

#[test]
fn lock_bts_to_memory_executes() {
    // lock bts [0x40], ax (0xf0 0x0f 0xab 0x06 0x40 0x00). ax=3 -> set bit 3 of [0x40].
    let mut memory = vec![0; 128];
    memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xab, 0x06, 0x40, 0x00]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0003);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
    assert!(!cpu.flag(FLAG_CF)); // old bit was 0
}

#[test]
fn lock_on_register_destination_delivers_ud() {
    // lock add ax, bx (0xf0 0x01 0xd8, mod=3 register dest). LOCK needs memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x01, 0xd8]);
    memory[0x18] = 0xee; // IVT[6] -> IP 0x00ee, CS 0
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_xchg_register_delivers_ud() {
    // lock xchg ax, bx (0xf0 0x87 0xd8, mod=3). XCHG needs memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x87, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_inc_register_delivers_ud() {
    // lock inc al (0xf0 0xfe 0xc0, FE /0 mod=3). INC of a register under LOCK -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0xfe, 0xc0]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_cmp_memory_delivers_ud() {
    // lock cmp [0x40], ax (0xf0 0x39 0x06 0x40 0x00). CMP is not lockable even to memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0xf0, 0x39, 0x06, 0x40, 0x00]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_non_lockable_opcode_delivers_ud() {
    // lock mov ax, bx (0xf0 0x89 0xd8). MOV is not lockable -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x89, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_bts_imm_to_memory_executes() {
    // lock bts [0x40], 3 (0xf0 0x0f 0xba 0x2e 0x40 0x00 0x03, /5 = BTS). set bit 3 of [0x40].
    let mut memory = vec![0; 128];
    memory[0..7].copy_from_slice(&[0xf0, 0x0f, 0xba, 0x2e, 0x40, 0x00, 0x03]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
    assert!(!cpu.flag(FLAG_CF)); // old bit was 0
}

#[test]
fn lock_btc_imm_register_delivers_ud() {
    // lock btc bx, 5 (0xf0 0x0f 0xba 0xfb 0x05, /7 = BTC, mod=3 register dest) -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0xf0, 0x0f, 0xba, 0xfb, 0x05]);
    memory[0x18] = 0xee;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn bswap_reverses_dword_byte_order() {
    // bswap eax (0x0f 0xc8). eax = 0x12345678 -> 0x78563412 in 32-bit operand mode.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x0f, 0xc8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    // A 32-bit code segment so the default operand size is dword.
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.write_gpr32(0, 0x1234_5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x7856_3412);
    // A second BSWAP restores the original (round-trip).
    cpu.registers.eip = 0;
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_gpr32(0), 0x1234_5678);
}

#[test]
fn invd_and_wbinvd_noop_at_cpl0() {
    // invd (0x0f 0x08) then wbinvd (0x0f 0x09) in real mode (CPL 0). Both are no-ops:
    // they advance past their two bytes and touch no register or flag.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0x08, 0x0f, 0x09]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 2);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn invd_at_cpl3_delivers_ud() {
    // invd (0x0f 0x08) at CPL 3 in protected mode raises #UD (vector 6).
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
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x08, 0, 0]);

    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn wbinvd_at_cpl3_delivers_ud() {
    // wbinvd (0x0f 0x09) at CPL 3 in protected mode raises #UD (vector 6).
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
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x09, 0, 0]);

    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn invlpg_memory_noop_at_cpl0() {
    // invlpg [0x40] (0x0f 0x01 0x3e 0x40 0x00, /7 with a memory operand) in real mode.
    // No TLB is modeled, so it is a no-op that advances past its bytes and leaves the
    // pointed-at memory untouched.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0x01, 0x3e, 0x40, 0x00]);
    memory[0x40] = 0xaa;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 5);
    assert_eq!(bus.memory[0x40], 0xaa);
}

#[test]
fn invlpg_at_cpl3_delivers_ud() {
    // invlpg [0x40] at CPL 3 in protected mode raises #UD (vector 6).
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
    cpu.registers.set_segment(
        SegmentIndex::Ds,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0x3e, 0x40, 0x00, 0, 0, 0]);

    // INVLPG (0F 01 /7) is converted to the decode/execute split (task A12); run it through the
    // split, where the CPL-3 #UD is raised in `execute_system_seg_decoded`.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn invlpg_register_form_delivers_ud() {
    // 0F 01 /7 with a register operand (mod=3) is #UD. ModRM 0xff = mod 3, reg 7, rm 7.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0xff, 0, 0]);

    // INVLPG (0F 01 /7) is converted to the decode/execute split (task A12); the register-form
    // (mod=3) #UD is raised in `execute_system_seg_decoded`.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn hardware_irq_injects_when_if_enabled() {
    // IVT[8] (physical 0x20) -> IP 0x00cc, CS 0. With IF=1 and a pending IRQ,
    // cycle() vectors to the handler before the NOP at eip 0 can execute.
    let mut memory = vec![0; 1024];
    memory[0] = 0x90; // nop that must NOT run
    memory[0x20] = 0xcc; // handler IP low byte
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    bus.pending_irq = Some(8);

    let outcome = cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00cc);
    assert!(!cpu.flag(FLAG_IF)); // delivery clears IF
    assert!(!outcome.halted);
    assert_eq!(bus.acknowledge_interrupt(), None); // the request was consumed
}

#[test]
fn hardware_irq_held_off_when_if_clear() {
    // IF=0: the pending IRQ waits and the NOP at eip 0 runs instead.
    let mut memory = vec![0; 1024];
    memory[0] = 0x90; // nop
    memory[0x20] = 0xcc;
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);
    bus.pending_irq = Some(8);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 1); // NOP executed, no vector taken
    assert_eq!(bus.acknowledge_interrupt(), Some(8)); // still pending
}

#[test]
fn hlt_wakes_on_pending_irq() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xf4; // hlt
    memory[0x20] = 0xcc; // IVT[8] IP
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    // First cycle executes HLT and halts.
    assert!(cpu.cycle(&mut bus).unwrap().halted);

    // A pending IRQ wakes the CPU and is delivered on the next cycle.
    bus.pending_irq = Some(8);
    let woken = cpu.cycle(&mut bus).unwrap();
    assert!(!woken.halted);
    assert_eq!(cpu.registers.eip, 0x00cc);
}

#[test]
fn hlt_stays_halted_without_deliverable_irq() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xf4; // hlt
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // execute HLT

    // No pending IRQ: stays halted.
    assert!(cpu.cycle(&mut bus).unwrap().halted);

    // Pending IRQ but IF=0: masked at the CPU, stays halted.
    cpu.set_flag(FLAG_IF, false);
    bus.pending_irq = Some(8);
    assert!(cpu.cycle(&mut bus).unwrap().halted);
}

#[test]
fn hlt_at_cpl0_protected_mode_halts() {
    // HLT is privileged (CPL 0 only), but ring 0 in protected mode is exactly
    // as permitted as real mode: require_cpl0 must not fault here.
    let mut memory = vec![0u8; 256];
    memory[0] = 0xf4; // hlt
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008, // RPL 0
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    assert!(cpu.cycle(&mut bus).unwrap().halted);
}

#[test]
fn hlt_at_cpl3_protected_mode_is_general_protection() {
    // Outside V86, a ring-3 HLT is the ordinary CPL check: #GP(0), same shape
    // as the existing CPL3 system-instruction tests.
    let (mut cpu, mut bus) = cpl3_code(&[0xf4]);
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
fn v86_guest_hlt_is_general_protection() {
    // A V86 task is always CPL 3 (current_privilege_level), so the guest's own
    // HLT now traps to the monitor instead of halting the machine directly
    // (the companion behavior tokaemm.asm's `.hlt` handler emulates).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "a V86 guest's HLT must land in the ring-0 monitor, not halt directly"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

#[test]
fn v86_guest_hlt_resumes_after_the_f4_byte_under_monitor_emulation() {
    // A monitor that emulates the trapped HLT (tokaemm.asm's `.hlt`: advance
    // past the F4 byte, then IRET back to V86) must land the guest one byte
    // past its HLT, still running, rather than leaving it stuck re-faulting on
    // the same instruction.
    let guest = [0xf4, 0x90]; // hlt ; nop
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    cpu.cycle(&mut bus).unwrap(); // guest HLT traps into the monitor
    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.cs().selector, R0_CS);

    // Emulate tokaemm.asm's `.hlt`: skip the error code, bump the frame's V86
    // EIP past the single-byte F4, then IRET back to V86 (mirrors the trap
    // round-trip in v86_monitor_round_trip_go_no_go).
    let esp = cpu.registers.esp() + 4;
    cpu.registers.set_esp(esp);
    let guest_eip = u32::from_le_bytes(cpu_mem(&bus, esp));
    assert_eq!(guest_eip, 0, "faulted at the guest's HLT");
    bus.memory[esp as usize..esp as usize + 4].copy_from_slice(&(guest_eip + 1).to_le_bytes());
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "IRET must return the guest to V86");
    assert_eq!(cpu.registers.eip, 1, "guest resumes past the HLT byte");
}

// --- 486 read-modify-write opcodes: XADD and CMPXCHG ---

#[test]
fn xadd_byte_swaps_and_adds_with_add_flags() {
    // 0F C0 /r XADD r/m8, r8. ModRM C3: mode 3, reg = AL(0), rm = BL(3).
    // dest = BL, src = AL. After: BL = BL + AL, AL = old BL, flags like ADD(BL, AL).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xc0, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x01); // AL (src)
    cpu.write_gpr8(3, 0xff); // BL (dest)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 0x00); // dest = 0xff + 0x01
    assert_eq!(cpu.read_gpr8(0), 0xff); // src = old dest
    // 0xff + 0x01 wraps to 0 with carry, half-carry, and a zero result.
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn xadd_word_matches_add_flags() {
    // 0F C1 /r XADD r/m16, r16. ModRM C3: reg = AX(0), rm = BX(3).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xc1, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x7fff); // AX (src)
    cpu.write_gpr16(3, 0x0001); // BX (dest)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr16(3), 0x8000); // 0x0001 + 0x7fff
    assert_eq!(cpu.read_gpr16(0), 0x0001); // old dest
    // Signed overflow: positive + positive crossed into the sign bit.
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn xadd_dword_matches_add_flags() {
    // 66h is not needed: with a 32-bit operand prefix on a real-mode CS, 66 0F C1 /r.
    // ModRM C3: reg = EAX(0), rm = EBX(3).
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xc1, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x1111_1111); // src
    cpu.registers.set_ebx(0x2222_2222); // dest
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ebx(), 0x3333_3333);
    assert_eq!(cpu.registers.eax(), 0x2222_2222);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn cmpxchg_byte_equal_stores_source() {
    // 0F B0 /r CMPXCHG r/m8, r8. ModRM C3: reg = CL(1, src), rm = BL(3, dest).
    // AL == BL so ZF is set and the source (CL) is stored into BL; AL is unchanged.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x42); // AL (accumulator)
    cpu.write_gpr8(3, 0x42); // BL (dest), equal to AL
    cpu.write_gpr8(1, 0x99); // CL (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF)); // equal compare
    assert_eq!(cpu.read_gpr8(3), 0x99); // dest = src
    assert_eq!(cpu.read_gpr8(0), 0x42); // accumulator unchanged
}

#[test]
fn cmpxchg_byte_unequal_loads_destination() {
    // AL != BL: ZF clear, AL = BL, BL unchanged.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x42); // AL
    cpu.write_gpr8(3, 0x10); // BL (dest), not equal
    cpu.write_gpr8(1, 0x99); // CL (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF)); // unequal compare
    assert_eq!(cpu.read_gpr8(0), 0x10); // accumulator = dest
    assert_eq!(cpu.read_gpr8(3), 0x10); // dest unchanged
    // Flags must match CMP(0x42, 0x10) = 0x32: no borrow, positive, nonzero.
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn cmpxchg_word_equal_stores_source() {
    // 0F B1 /r CMPXCHG r/m16, r16. ModRM C3: reg = CX(1, src), rm = BX(3, dest).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb1, 0xcb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x1234); // AX
    cpu.write_gpr16(3, 0x1234); // BX, equal
    cpu.write_gpr16(1, 0xbeef); // CX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_gpr16(3), 0xbeef);
    assert_eq!(cpu.read_gpr16(0), 0x1234);
}

#[test]
fn cmpxchg_dword_unequal_loads_destination() {
    // 66 0F B1 /r CMPXCHG r/m32, r32. ModRM C3: reg = ECX(1, src), rm = EBX(3, dest).
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb1, 0xcb]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xaaaa_aaaa); // EAX
    cpu.registers.set_ebx(0x5555_5555); // EBX (dest), not equal
    cpu.registers.set_ecx(0xdead_beef); // ECX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.eax(), 0x5555_5555); // accumulator = dest
    assert_eq!(cpu.registers.ebx(), 0x5555_5555); // dest unchanged
}

#[test]
fn lock_xadd_to_memory_is_accepted() {
    // F0 0F C1 06 00 02: LOCK XADD [0x0200], AX. ModRM 06 is mode 0 rm 6 (direct disp16),
    // a memory destination, so the LOCK is legal and the instruction runs.
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0x06, 0x00, 0x02]);
    memory[0x200..0x202].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x0001); // AX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // [0x0200] = 0x0010 + 0x0001, AX = old [0x0200].
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x0011
    );
    assert_eq!(cpu.read_gpr16(0), 0x0010);
}

#[test]
fn lock_xadd_to_register_is_undefined_opcode() {
    // F0 0F C1 C3: LOCK XADD BX, AX. The register destination makes the LOCK prefix illegal,
    // so the decoder raises #UD (vector 6) before executing.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0xc3]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn lock_bswap_is_undefined_opcode() {
    // F0 0F C8: LOCK BSWAP EAX. BSWAP has no memory form, so LOCK is always #UD.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xf0, 0x0f, 0xc8]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

// Build a CPL-3 protected-mode CPU whose CS and DS are flat user segments, running
// MOV AX, moffs16 (0xa1) that reads a word from DS:moffs. The caller picks the
// moffs so the access lands on an even or odd boundary.
fn cpl3_word_read_at(moffs: u16) -> (CpuGsw, TestBus) {
    let mut memory = vec![0; 256];
    memory[0] = 0xa1;
    memory[1..3].copy_from_slice(&moffs.to_le_bytes());
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
    cpu.registers.set_segment(
        SegmentIndex::Ds,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    (cpu, TestBus::with_memory(memory))
}

#[test]
fn misaligned_word_read_faults_ac_when_am_and_ac_set_at_cpl3() {
    // CR0.AM and EFLAGS.AC both set, CPL 3, odd word address: #AC (vector 17, no
    // error code).
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);

    // 0xa1 (MOV AX, moffs) is converted to the split, so drive it through the split executor;
    // the legacy fused entry no longer carries that arm. The #AC alignment check fires in the
    // shared memory-read helper either way.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 17,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn misaligned_word_read_no_fault_without_cr0_am() {
    // EFLAGS.AC set but CR0.AM clear: the alignment check stays masked, no fault.
    // Set CR0 bit 4 (ET) too: it is not AM, so it must not arm the check.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= 0x0000_0010; // bit 4 (ET), not AM
    cpu.set_flag(FLAG_AC, true);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn misaligned_word_read_no_fault_without_eflags_ac() {
    // CR0.AM set but EFLAGS.AC clear: software has not opted in, no fault.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn misaligned_word_read_no_fault_at_supervisor() {
    // AM and AC both set, but CPL 0 (supervisor): exempt, no fault. Reuse the
    // CPL-3 setup and drop CS/DS RPL to 0.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);
    let mut cs = cpu.registers.cs();
    cs.selector = 0x0000;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.selector = 0x0000;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu.cpl = 0; // dropped CS's RPL to 0 above; seed the cached CPL to match

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn aligned_word_read_never_faults_with_am_and_ac() {
    // Even word address: aligned, so no #AC even with AM and AC set at CPL 3.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0040);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn eflags_ac_and_id_survive_pushf_popf_round_trip() {
    // 66 9c PUSHFD ; 66 9d POPFD. Set AC and ID, perturb both after they reach the
    // stack, and confirm POPFD restores them from the dword flag image.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    memory[2..4].copy_from_slice(&[0x66, 0x9d]);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_AC, true);
    cpu.set_flag(FLAG_ID, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pushfd
    cpu.set_flag(FLAG_AC, false); // perturb after the image is on the stack
    cpu.set_flag(FLAG_ID, false);
    cpu.cycle(&mut bus).unwrap(); // popfd

    assert!(cpu.flag(FLAG_AC));
    assert!(cpu.flag(FLAG_ID));
}
