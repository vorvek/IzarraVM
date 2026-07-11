// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn push_pop_known_bytes() {
    let mut e = Encoder::new();
    e.push(Reg::RBX);
    e.push(Reg::R12);
    e.push(Reg::R13);
    e.push(Reg::R14);
    e.push(Reg::R15);
    e.pop(Reg::R15);
    assert_eq!(
        e.finish(),
        vec![
            0x53, 0x41, 0x54, 0x41, 0x55, 0x41, 0x56, 0x41, 0x57, 0x41, 0x5F
        ]
    );
}

#[test]
fn mov_r64_r64_known_bytes() {
    // mov r12, rcx ; mov r13, rdx ; mov r15, r8 ; mov rbx, r9
    let mut e = Encoder::new();
    e.mov_r64_r64(Reg::R12, Reg::RCX);
    e.mov_r64_r64(Reg::R13, Reg::RDX);
    e.mov_r64_r64(Reg::R15, Reg::R8);
    e.mov_r64_r64(Reg::RBX, Reg::R9);
    assert_eq!(
        e.finish(),
        vec![
            0x49, 0x89, 0xCC, 0x49, 0x89, 0xD5, 0x4D, 0x89, 0xC7, 0x4C, 0x89, 0xCB
        ]
    );
}

#[test]
fn direct_store_bookkeeping_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.or_r64_r64(Reg::RAX, Reg::RDX);
    e.load_r32_sib_scale4(Reg::RDX, Reg::RDI, Reg::RCX);
    e.cmp_r8_disp8(Reg::RDX, Reg::RDI, 0);
    e.add_r64_to_mem_disp8(Reg::RSP, 0, Reg::RDX);
    e.bt_r64_mem(Reg::RDX, Reg::RCX);
    e.bts_r64_mem(Reg::RSP, Reg::RDX);
    assert_eq!(
        e.finish(),
        vec![
            0x48, 0x09, 0xD0, // or rax,rdx
            0x8B, 0x14, 0x8F, // mov edx,[rdi+rcx*4]
            0x3A, 0x57, 0x00, // cmp dl,[rdi]
            0x48, 0x01, 0x54, 0x24, 0x00, // add qword [rsp],rdx
            0x48, 0x0F, 0xA3, 0x0A, // bt qword [rdx],rcx
            0x48, 0x0F, 0xAB, 0x14, 0x24, // bts qword [rsp],rdx
        ]
    );
}

#[test]
fn scalar_sse_memory_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.movsd_xmm_disp32(Xmm::XMM1, Reg::R12, 0x1122_3344);
    e.movsd_disp32_xmm(Reg::R13, -4, Xmm::XMM9);
    e.movss_xmm_disp32(Xmm::XMM10, Reg::RSP, 8);
    e.movss_disp32_xmm(Reg::RDI, 0, Xmm::XMM3);
    assert_eq!(
        e.finish(),
        vec![
            0xF2, 0x41, 0x0F, 0x10, 0x8C, 0x24, 0x44, 0x33, 0x22, 0x11, 0xF2, 0x45, 0x0F, 0x11,
            0x8D, 0xFC, 0xFF, 0xFF, 0xFF, 0xF3, 0x44, 0x0F, 0x10, 0x94, 0x24, 0x08, 0x00, 0x00,
            0x00, 0xF3, 0x0F, 0x11, 0x9F, 0x00, 0x00, 0x00, 0x00,
        ]
    );
}

#[test]
fn indexed_movsd_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.movsd_xmm_sib_scale8_disp32(Xmm::XMM2, Reg::R15, Reg::R9, 0x20);
    e.movsd_sib_scale8_disp32_xmm(Reg::R12, Reg::R10, -8, Xmm::XMM11);
    assert_eq!(
        e.finish(),
        vec![
            0xF2, 0x43, 0x0F, 0x10, 0x94, 0xCF, 0x20, 0x00, 0x00, 0x00, 0xF2, 0x47, 0x0F, 0x11,
            0x9C, 0xD4, 0xF8, 0xFF, 0xFF, 0xFF,
        ]
    );
}

#[test]
fn scalar_sse_arithmetic_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.movsd_xmm_xmm(Xmm::XMM1, Xmm::XMM2);
    e.cvtss2sd(Xmm::XMM1, Xmm::XMM2);
    e.cvtsd2ss(Xmm::XMM9, Xmm::XMM10);
    e.addsd(Xmm::XMM0, Xmm::XMM1);
    e.mulsd(Xmm::XMM2, Xmm::XMM3);
    e.subsd(Xmm::XMM8, Xmm::XMM9);
    e.divsd(Xmm::XMM10, Xmm::XMM11);
    e.sqrtsd(Xmm::XMM12, Xmm::XMM13);
    e.ucomisd(Xmm::XMM14, Xmm::XMM15);
    e.xorpd(Xmm::XMM1, Xmm::XMM2);
    assert_eq!(
        e.finish(),
        vec![
            0xF2, 0x0F, 0x10, 0xCA, 0xF3, 0x0F, 0x5A, 0xCA, 0xF2, 0x45, 0x0F, 0x5A, 0xCA, 0xF2,
            0x0F, 0x58, 0xC1, 0xF2, 0x0F, 0x59, 0xD3, 0xF2, 0x45, 0x0F, 0x5C, 0xC1, 0xF2, 0x45,
            0x0F, 0x5E, 0xD3, 0xF2, 0x45, 0x0F, 0x51, 0xE5, 0x66, 0x45, 0x0F, 0x2E, 0xF7, 0x66,
            0x0F, 0x57, 0xCA,
        ]
    );
}

#[test]
fn variable_shift_form_has_known_bytes() {
    let mut e = Encoder::new();
    e.shr_r32_cl(Reg::RDX);
    e.shr_r32_cl(Reg::R9);
    assert_eq!(e.finish(), vec![0xD3, 0xEA, 0x41, 0xD3, 0xE9]);
}

#[test]
fn double_shift_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.double_shift_r32(true, Reg::RAX, Reg::RDX, Some(31));
    e.double_shift_r32(true, Reg::R8, Reg::R9, None);
    e.double_shift_r32(false, Reg::RCX, Reg::RBX, Some(33));
    e.double_shift_r32(false, Reg::R10, Reg::R11, None);
    assert_eq!(
        e.finish(),
        vec![
            0x0F, 0xA4, 0xD0, 0x1F, 0x45, 0x0F, 0xA5, 0xC8, 0x0F, 0xAC, 0xD9, 0x21, 0x45, 0x0F,
            0xAD, 0xDA,
        ]
    );
}

#[test]
fn scalar_sse_integer_conversion_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.cvtsi2sd_r32(Xmm::XMM1, Reg::RAX);
    e.cvtsi2sd_r32(Xmm::XMM9, Reg::R10);
    e.cvttsd2si_r32(Reg::RAX, Xmm::XMM1);
    e.cvttsd2si_r32(Reg::R9, Xmm::XMM10);
    e.cvttsd2si_r64(Reg::RAX, Xmm::XMM1);
    e.cvttsd2si_r64(Reg::R9, Xmm::XMM10);
    assert_eq!(
        e.finish(),
        vec![
            0xF2, 0x0F, 0x2A, 0xC8, 0xF2, 0x45, 0x0F, 0x2A, 0xCA, 0xF2, 0x0F, 0x2C, 0xC1, 0xF2,
            0x45, 0x0F, 0x2C, 0xCA, 0xF2, 0x48, 0x0F, 0x2C, 0xC1, 0xF2, 0x4D, 0x0F, 0x2C, 0xCA,
        ]
    );
}

#[test]
fn movq_and_movd_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.movq_xmm_r64(Xmm::XMM1, Reg::RAX);
    e.movq_xmm_r64(Xmm::XMM9, Reg::R10);
    e.movq_r64_xmm(Reg::RAX, Xmm::XMM1);
    e.movq_r64_xmm(Reg::R10, Xmm::XMM9);
    e.movd_xmm_r32(Xmm::XMM1, Reg::RAX);
    e.movd_xmm_r32(Xmm::XMM9, Reg::R10);
    e.movd_r32_xmm(Reg::RAX, Xmm::XMM1);
    e.movd_r32_xmm(Reg::R10, Xmm::XMM9);
    assert_eq!(
        e.finish(),
        vec![
            0x66, 0x48, 0x0F, 0x6E, 0xC8, 0x66, 0x4D, 0x0F, 0x6E, 0xCA, 0x66, 0x48, 0x0F, 0x7E,
            0xC8, 0x66, 0x4D, 0x0F, 0x7E, 0xCA, 0x66, 0x0F, 0x6E, 0xC8, 0x66, 0x45, 0x0F, 0x6E,
            0xCA, 0x66, 0x0F, 0x7E, 0xC8, 0x66, 0x45, 0x0F, 0x7E, 0xCA,
        ]
    );
}

#[test]
fn x87_integer_support_forms_have_known_bytes() {
    let mut e = Encoder::new();
    e.and_r64_r64(Reg::RAX, Reg::RDX);
    e.xor_r64_r64(Reg::R9, Reg::R10);
    e.shift_r64_imm8(5, Reg::R8, 52);
    e.shift_r64_imm8(4, Reg::RDX, 1);
    e.movzx_r32_word_disp32(Reg::RCX, Reg::R12, 0x1234);
    e.store_r16_disp32(Reg::R13, -2, Reg::R9);
    e.bt_r16_mem(Reg::RDX, Reg::RCX);
    e.btr_r16_mem(Reg::R12, Reg::R9);
    e.bts_r16_mem(Reg::RDI, Reg::RCX);
    assert_eq!(
        e.finish(),
        vec![
            0x48, 0x21, 0xD0, 0x4D, 0x31, 0xD1, 0x49, 0xC1, 0xE8, 0x34, 0x48, 0xC1, 0xE2, 0x01,
            0x41, 0x0F, 0xB7, 0x8C, 0x24, 0x34, 0x12, 0x00, 0x00, 0x66, 0x45, 0x89, 0x8D, 0xFE,
            0xFF, 0xFF, 0xFF, 0x66, 0x0F, 0xA3, 0x0A, 0x66, 0x45, 0x0F, 0xB3, 0x0C, 0x24, 0x66,
            0x0F, 0xAB, 0x0F,
        ]
    );
}

#[test]
fn mov_r32_r32_known_bytes() {
    // Non-extended pair: mov eax, ecx -- no REX byte at all.
    let mut e = Encoder::new();
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    assert_eq!(e.finish(), vec![0x89, 0xC8]);

    // Extended pair: mov r12d, r9d -- REX present (R from src ext, B from dst ext).
    let mut e = Encoder::new();
    e.mov_r32_r32(Reg::R12, Reg::R9);
    assert_eq!(e.finish(), vec![0x45, 0x89, 0xCC]);
}

#[test]
fn mov_r32_imm32_known_bytes() {
    // mov r9d, 0x12345678 -- REX.B set (R9 is extended), then B8+1, then imm32 LE.
    let mut e = Encoder::new();
    e.mov_r32_imm32(Reg::R9, 0x1234_5678);
    assert_eq!(e.finish(), vec![0x41, 0xB9, 0x78, 0x56, 0x34, 0x12]);
}

#[test]
fn mov_r64_imm64_known_bytes() {
    // mov rbx, 0x0102030405060708 -- REX.W only (RBX not extended), B8+3, imm64 LE.
    let mut e = Encoder::new();
    e.mov_r64_imm64(Reg::RBX, 0x0102_0304_0506_0708);
    assert_eq!(
        e.finish(),
        vec![0x48, 0xBB, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
    );
}

#[test]
fn cmp_r64_imm32_known_bytes() {
    // cmp rcx, 10 -- ModRM reg field is /7, distinct from add's /0 and sub's /5.
    let mut e = Encoder::new();
    e.cmp_r64_imm32(Reg::RCX, 10);
    assert_eq!(e.finish(), vec![0x48, 0x81, 0xF9, 0x0A, 0x00, 0x00, 0x00]);
}

#[test]
fn xor_self_known_bytes() {
    let mut e = Encoder::new();
    e.xor_r64_self(Reg::R14);
    assert_eq!(e.finish(), vec![0x4D, 0x31, 0xF6]);
}

#[test]
fn sub_add_rsp_known_bytes() {
    let mut e = Encoder::new();
    e.sub_r64_imm32(Reg::RSP, 32);
    e.add_r64_imm32(Reg::RSP, 32);
    assert_eq!(
        e.finish(),
        vec![
            0x48, 0x81, 0xEC, 0x20, 0x00, 0x00, 0x00, 0x48, 0x81, 0xC4, 0x20, 0x00, 0x00, 0x00,
        ]
    );
}

#[test]
fn load_disp8_known_bytes() {
    let mut e = Encoder::new();
    // mov rax, [r15+0]
    e.load_r64_disp8(Reg::RAX, Reg::R15, 0);
    assert_eq!(e.finish(), vec![0x49, 0x8B, 0x47, 0x00]);
}

#[test]
fn store_disp8_known_bytes() {
    let mut e = Encoder::new();
    // mov [r15+32], rax
    e.store_r64_disp8(Reg::R15, 32, Reg::RAX);
    assert_eq!(e.finish(), vec![0x49, 0x89, 0x47, 0x20]);
}

#[test]
fn load_r32_disp8_known_bytes() {
    // mov eax, [r15+0] -- REX.B only (r15 extended), no REX.W (32-bit operand). The 32-bit load
    // zero-extends to 64 bits, exactly what the inline slot wants when reading a guest gpr.
    let mut e = Encoder::new();
    e.load_r32_disp8(Reg::RAX, Reg::R15, 0);
    assert_eq!(e.finish(), vec![0x41, 0x8B, 0x47, 0x00]);
}

#[test]
fn store_r32_disp8_known_bytes() {
    // mov [r15+32], eax -- REX.B only (r15 extended), no REX.W.
    let mut e = Encoder::new();
    e.store_r32_disp8(Reg::R15, 32, Reg::RAX);
    assert_eq!(e.finish(), vec![0x41, 0x89, 0x47, 0x20]);
}

#[test]
fn load_store_r32_disp32_known_bytes() {
    // mov eax, [r14+128] -- REX.B (r14 extended), no REX.W. 8B; ModRM mod=10,reg=eax(0),
    // rm=r14&7=6 = 10_000_110 = 0x86; disp32 128 LE.
    let mut e = Encoder::new();
    e.load_r32_disp32(Reg::RAX, Reg::R14, 128);
    assert_eq!(e.finish(), vec![0x41, 0x8B, 0x86, 0x80, 0x00, 0x00, 0x00]);
    // mov [r14+128], eax -- 89; ModRM 0x86; same disp32.
    let mut e = Encoder::new();
    e.store_r32_disp32(Reg::R14, 128, Reg::RAX);
    assert_eq!(e.finish(), vec![0x41, 0x89, 0x86, 0x80, 0x00, 0x00, 0x00]);
    // mov eax, [r12+200] -- r12&7=4 forces a SIB byte (0x24); REX.B. ModRM mod=10 = 0x84.
    let mut e = Encoder::new();
    e.load_r32_disp32(Reg::RAX, Reg::R12, 200);
    assert_eq!(
        e.finish(),
        vec![0x41, 0x8B, 0x84, 0x24, 0xC8, 0x00, 0x00, 0x00]
    );
}

#[test]
fn movzx_r32_byte_disp32_known_bytes() {
    let mut e = Encoder::new();
    e.movzx_r32_byte_disp32(Reg::RAX, Reg::R12, 0x1234);
    assert_eq!(
        e.finish(),
        vec![0x41, 0x0f, 0xb6, 0x84, 0x24, 0x34, 0x12, 0x00, 0x00]
    );
}

#[test]
fn store_then_load_r32_disp32_round_trips() {
    // End-to-end: reserve stack, store a dword to [rsp+128] via disp32, reload it into a
    // different register, return it. Exercises the SIB-forced (RSP) disp32 store+load path.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    e.sub_r64_imm32(Reg::RSP, 256);
    e.mov_r32_imm32(Reg::RAX, 0x1234_5678);
    e.store_r32_disp32(Reg::RSP, 128, Reg::RAX);
    e.mov_r32_imm32(Reg::RAX, 0); // clobber so the reload is observable
    e.load_r32_disp32(Reg::RCX, Reg::RSP, 128);
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    e.add_r64_imm32(Reg::RSP, 256);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(), 0x1234_5678);
}

#[test]
fn load_r32_sib_known_bytes() {
    // mov r11d, [r12 + r9] -- REX.W=0,R=1(r11),X=1(r9 index),B=1(r12 base) = 0100_0111 = 0x47;
    // 8B; ModRM mod=00,reg=r11&7=3,rm=100(SIB) = 00_011_100 = 0x1C; SIB scale=0,index=r9&7=1,
    // base=r12&7=4 = 00_001_100 = 0x0C.
    let mut e = Encoder::new();
    e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9);
    assert_eq!(e.finish(), vec![0x47, 0x8B, 0x1C, 0x0C]);

    // mov eax, [esi + ecx] -- no extended regs, so no REX. 8B; ModRM mod=00,reg=eax(0),rm=100 =
    // 0x04; SIB scale=0,index=ecx(1),base=esi(6) = 00_001_110 = 0x0E. Matches the guest bytes
    // `8B 04 0E` the probe's interpreter side executes for the same operation.
    let mut e = Encoder::new();
    e.load_r32_sib(Reg::RAX, Reg::RSI, Reg::RCX);
    assert_eq!(e.finish(), vec![0x8B, 0x04, 0x0E]);
}

#[test]
fn load_r64_sib_scale8_known_bytes() {
    let mut e = Encoder::new();
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDX, Reg::RCX);
    assert_eq!(e.finish(), vec![0x48, 0x8B, 0x3C, 0xCA]);

    let mut e = Encoder::new();
    e.load_r64_sib_scale8(Reg::R10, Reg::R11, Reg::R9);
    assert_eq!(e.finish(), vec![0x4F, 0x8B, 0x14, 0xCB]);
}

#[test]
fn add_r32_imm32_known_bytes() {
    // add eax, 0xa0 -- no REX (eax not extended), 81 /0 id. ModRM mod=11,reg=0(/0),rm=0(eax)
    // = 11_000_000 = 0xC0.
    let mut e = Encoder::new();
    e.add_r32_imm32(Reg::RAX, 0xa0);
    assert_eq!(e.finish(), vec![0x81, 0xC0, 0xA0, 0x00, 0x00, 0x00]);
}

#[test]
fn shr_r32_imm8_known_bytes() {
    // shr ecx, 25 -- no REX (ecx not extended), C1 /5 ib. ModRM mod=11,reg=5(/5),rm=1(ecx)
    // = 11_101_001 = 0xE9; count 25 = 0x19. This is the drawcolumn shift slot.
    let mut e = Encoder::new();
    e.shr_r32_imm8(Reg::RCX, 25);
    assert_eq!(e.finish(), vec![0xC1, 0xE9, 0x19]);
}

#[test]
fn and_r32_imm32_known_bytes() {
    // and eax, 0xfffff000 -- no REX, 81 /4 id. ModRM mod=11,reg=4(/4),rm=0(eax) = 0xE0.
    let mut e = Encoder::new();
    e.and_r32_imm32(Reg::RAX, 0xffff_f000);
    assert_eq!(e.finish(), vec![0x81, 0xE0, 0x00, 0xF0, 0xFF, 0xFF]);
    // and r12d, 0xff -- REX.B (r12 extended). ModRM rm=r12&7=4 -> 0xE4.
    let mut e = Encoder::new();
    e.and_r32_imm32(Reg::R12, 0xff);
    assert_eq!(e.finish(), vec![0x41, 0x81, 0xE4, 0xFF, 0x00, 0x00, 0x00]);
}

#[test]
fn cmp_r32_imm32_known_bytes() {
    // cmp eax, 3 -- no REX, 81 /7 id. ModRM 11_111_000 = 0xF8.
    let mut e = Encoder::new();
    e.cmp_r32_imm32(Reg::RAX, 3);
    assert_eq!(e.finish(), vec![0x81, 0xF8, 0x03, 0x00, 0x00, 0x00]);
    // cmp r12d, 0xff -- REX.B.
    let mut e = Encoder::new();
    e.cmp_r32_imm32(Reg::R12, 0xff);
    assert_eq!(e.finish(), vec![0x41, 0x81, 0xFC, 0xFF, 0x00, 0x00, 0x00]);
}

#[test]
fn or_r32_r32_known_bytes() {
    // or eax, ecx -- no REX, 09 /r. ModRM mod=11,reg=ecx(1),rm=eax(0) = 0xC8.
    let mut e = Encoder::new();
    e.or_r32_r32(Reg::RAX, Reg::RCX);
    assert_eq!(e.finish(), vec![0x09, 0xC8]);
    // or r12d, eax -- REX.B on dst (r12), reg=eax(0) so ModRM rm=4 (r12), reg=0 -> 0xC4 ; REX 0x41.
    let mut e = Encoder::new();
    e.or_r32_r32(Reg::R12, Reg::RAX);
    assert_eq!(e.finish(), vec![0x41, 0x09, 0xC4]);
}

#[test]
fn or_r32_imm32_known_bytes() {
    // or eax, 0x00000fff -- no REX, 81 /1 . ModRM 11_001_000 = 0xC8.
    let mut e = Encoder::new();
    e.or_r32_imm32(Reg::RAX, 0x0000_0fff);
    assert_eq!(e.finish(), vec![0x81, 0xC8, 0xFF, 0x0F, 0x00, 0x00]);
}

#[test]
fn shl_r32_imm8_known_bytes() {
    // shl eax, 4 -- no REX, C1 /4 ib (reg field /4, vs shr's /5). ModRM 11_100_000 = 0xE0.
    let mut e = Encoder::new();
    e.shl_r32_imm8(Reg::RAX, 4);
    assert_eq!(e.finish(), vec![0xC1, 0xE0, 0x04]);
}

#[test]
fn add_r32_r32_known_bytes() {
    // add eax, ecx -- no REX, 01 /r. ModRM mod=11,reg=ecx(1),rm=eax(0) = 0xC8.
    let mut e = Encoder::new();
    e.add_r32_r32(Reg::RAX, Reg::RCX);
    assert_eq!(e.finish(), vec![0x01, 0xC8]);
    // add r12d, r9d -- REX (R from src r9 ext, B from dst r12 ext) = 0x45. ModRM 11_001_100 = 0xCC.
    let mut e = Encoder::new();
    e.add_r32_r32(Reg::R12, Reg::R9);
    assert_eq!(e.finish(), vec![0x45, 0x01, 0xCC]);
}

#[test]
fn byte_alu_register_forms_have_known_bytes() {
    let mut e = Encoder::new();
    for op in 0..8 {
        e.alu_r8_r8(op, Reg::RAX, Reg::RCX);
    }
    assert_eq!(
        e.finish(),
        vec![
            0x00, 0xC8, 0x08, 0xC8, 0x10, 0xC8, 0x18, 0xC8, 0x20, 0xC8, 0x28, 0xC8, 0x30, 0xC8,
            0x38, 0xC8,
        ]
    );
}

#[test]
fn movzx_r32_byte_sib_known_bytes() {
    // movzx eax, byte [rsi+rcx] -- no REX. 0F B6; ModRM mod=00,reg=eax(0),rm=100(SIB)=0x04;
    // SIB scale=0,index=rcx(1),base=rsi(6) = 0x0E.
    let mut e = Encoder::new();
    e.movzx_r32_byte_sib(Reg::RAX, Reg::RSI, Reg::RCX);
    assert_eq!(e.finish(), vec![0x0F, 0xB6, 0x04, 0x0E]);
    // movzx r11d, byte [r12+r9] -- REX.R(r11)+X(r9)+B(r12) = 0x47; ModRM reg=r11&7=3,rm=100 = 0x1C;
    // SIB index=r9&7=1,base=r12&7=4 = 0x0C.
    let mut e = Encoder::new();
    e.movzx_r32_byte_sib(Reg::R11, Reg::R12, Reg::R9);
    assert_eq!(e.finish(), vec![0x47, 0x0F, 0xB6, 0x1C, 0x0C]);
}

#[test]
fn movzx_r32_byte_disp8_known_bytes() {
    let mut e = Encoder::new();
    e.movzx_r32_byte_disp8(Reg::RDX, Reg::RDI, 0);
    assert_eq!(e.finish(), vec![0x0F, 0xB6, 0x57, 0x00]);

    let mut e = Encoder::new();
    e.movzx_r32_byte_disp8(Reg::R9, Reg::R12, -4);
    assert_eq!(e.finish(), vec![0x45, 0x0F, 0xB6, 0x4C, 0x24, 0xFC]);
}

#[test]
fn store_r8_disp8_known_bytes() {
    // mov [r14+8], al -- REX.B (r14 extended). 88; ModRM mod=01,reg=al(0),rm=r14&7=6 = 0x46; disp 8.
    let mut e = Encoder::new();
    e.store_r8_disp8(Reg::R14, 8, Reg::RAX);
    assert_eq!(e.finish(), vec![0x41, 0x88, 0x46, 0x08]);
    // mov [r12+8], al -- r12&7=4 forces a SIB byte (0x24). REX.B. ModRM 0x44.
    let mut e = Encoder::new();
    e.store_r8_disp8(Reg::R12, 8, Reg::RAX);
    assert_eq!(e.finish(), vec![0x41, 0x88, 0x44, 0x24, 0x08]);
    // mov [rax+1], cl -- no REX, no SIB. ModRM mod=01,reg=cl(1),rm=rax(0) = 0x48; disp 1.
    let mut e = Encoder::new();
    e.store_r8_disp8(Reg::RAX, 1, Reg::RCX);
    assert_eq!(e.finish(), vec![0x88, 0x48, 0x01]);
}

#[test]
fn movzx_byte_sib_reads_the_right_byte() {
    // End-to-end: fn(base, idx) -> i64 returns the zero-extended byte at [base+idx]. Proves the
    // SIB addressing + zero-extension actually execute, not just the byte shape.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    #[cfg(windows)]
    {
        e.movzx_r32_byte_sib(Reg::RAX, Reg::RCX, Reg::RDX); // win64 arg0=RCX base, arg1=RDX idx
    }
    #[cfg(not(windows))]
    {
        e.movzx_r32_byte_sib(Reg::RAX, Reg::RDI, Reg::RSI); // sysv arg0=RDI base, arg1=RSI idx
    }
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn(*const u8, i64) -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    let data = [0x11u8, 0x22, 0x33, 0xAB, 0x55];
    assert_eq!(f(data.as_ptr(), 3), 0xAB);
    assert_eq!(f(data.as_ptr(), 0), 0x11);
}

#[test]
fn store_r8_writes_only_the_low_byte() {
    // End-to-end: fn(dst, val) stores the low byte of `val` to dst[1], leaving dst[0]/dst[2]
    // untouched -- the write_gpr8 byte-lane semantics the probe relies on.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    #[cfg(windows)]
    {
        e.mov_r32_r32(Reg::RAX, Reg::RDX); // val -> EAX (AL = low byte); dst is RCX
        e.store_r8_disp8(Reg::RCX, 1, Reg::RAX);
    }
    #[cfg(not(windows))]
    {
        e.mov_r32_r32(Reg::RAX, Reg::RSI); // val -> EAX (AL = low byte); dst is RDI
        e.store_r8_disp8(Reg::RDI, 1, Reg::RAX);
    }
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn(*mut u8, i64) = unsafe { std::mem::transmute(buf.entry_ptr()) };
    let mut data = [0xEEu8, 0xEE, 0xEE];
    f(data.as_mut_ptr(), 0x1234_567A);
    assert_eq!(
        data,
        [0xEE, 0x7A, 0xEE],
        "only dst[1]'s low byte should change"
    );
}

#[test]
fn cmp_r32_disp8_known_bytes() {
    // cmp ecx, [rdx+0] -- no REX, 3B /r. ModRM mod=01,reg=ecx(1),rm=rdx(2) = 0x4A; disp 0.
    let mut e = Encoder::new();
    e.cmp_r32_disp8(Reg::RCX, Reg::RDX, 0);
    assert_eq!(e.finish(), vec![0x3B, 0x4A, 0x00]);
    // cmp ecx, [r15+8] -- REX.B (r15 base extended). ModRM rm=r15&7=7 -> 0x4F; disp 8.
    let mut e = Encoder::new();
    e.cmp_r32_disp8(Reg::RCX, Reg::R15, 8);
    assert_eq!(e.finish(), vec![0x41, 0x3B, 0x4F, 0x08]);
    // cmp eax, [r12+0] -- r12&7=4 forces a SIB byte (0x24); REX.B. ModRM 0x44.
    let mut e = Encoder::new();
    e.cmp_r32_disp8(Reg::RAX, Reg::R12, 0);
    assert_eq!(e.finish(), vec![0x41, 0x3B, 0x44, 0x24, 0x00]);
}

#[test]
fn cmp_r32_disp8_reads_the_memory_dword() {
    // End-to-end: fn(ptr) -> i64 returns 1 iff *ptr == 0xCAFE, via cmp eax,[ptr] + jz. Proves the
    // memory operand reads the right dword and sets ZF, not just the byte shape.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    e.mov_r32_imm32(Reg::RAX, 0xCAFE);
    #[cfg(windows)]
    e.cmp_r32_disp8(Reg::RAX, Reg::RCX, 0); // win64 arg0 = RCX
    #[cfg(not(windows))]
    e.cmp_r32_disp8(Reg::RAX, Reg::RDI, 0); // sysv arg0 = RDI
    let hit = e.label();
    e.jz(hit);
    e.mov_r64_imm64(Reg::RAX, 0);
    let end = e.label();
    e.jmp(end);
    e.place(hit);
    e.mov_r64_imm64(Reg::RAX, 1);
    e.place(end);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn(*const u32) -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(&0xCAFEu32), 1);
    assert_eq!(f(&0x1234u32), 0);
}

#[test]
fn load_store_disp8_through_rsp_emits_a_sib_byte() {
    // RSP (low3 == 0b100) can NEVER be a ModRM base directly -- the encoding is reserved to
    // mean "a SIB byte follows" regardless of mod or REX.B. Omitting the SIB byte here would
    // silently shift every following byte by one and corrupt the rest of the instruction
    // stream (this is exactly the bug that originally broke the strcpy block's stack-scratch
    // spill/reload with a host access violation instead of a clean assertion failure).
    let mut e = Encoder::new();
    // mov [rsp+32], rax -- REX.W only (neither RSP nor RAX extended), opcode 89,
    // modrm mod=01,reg=rax&7=0,rm=rsp&7=4 -> 01_000_100 = 0x44, SIB 0x24, disp8 0x20.
    e.store_r64_disp8(Reg::RSP, 32, Reg::RAX);
    assert_eq!(e.finish(), vec![0x48, 0x89, 0x44, 0x24, 0x20]);

    // mov rcx, [rsp+32] -- modrm mod=01,reg=rcx&7=1,rm=rsp&7=4 -> 01_001_100 = 0x4C.
    let mut e = Encoder::new();
    e.load_r64_disp8(Reg::RCX, Reg::RSP, 32);
    assert_eq!(e.finish(), vec![0x48, 0x8B, 0x4C, 0x24, 0x20]);
}

#[test]
fn store_then_load_through_rsp_round_trips_a_real_value() {
    // A real end-to-end check, not just byte shape: emit a function that reserves stack space,
    // stores a value to `[rsp+32]`, reloads it into a different register, restores the stack,
    // and returns it. If the SIB byte were missing, the reload would read garbage (or the
    // emitted bytes would desync entirely and likely crash the process).
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    e.sub_r64_imm32(Reg::RSP, 48);
    e.mov_r64_imm64(Reg::RAX, 0x99);
    e.store_r64_disp8(Reg::RSP, 32, Reg::RAX);
    e.mov_r64_imm64(Reg::RAX, 0); // clobber RAX so the reload below is observable
    e.load_r64_disp8(Reg::RAX, Reg::RSP, 32);
    e.add_r64_imm32(Reg::RSP, 48);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(), 0x99);
}

#[test]
fn cmp_r64_r64_known_bytes() {
    // cmp r14, rbx -- REX.W=1,R=0(rbx not extended),B=1(r14 extended) = 0100_1001 = 0x49;
    // opcode 39; modrm mod=11,reg=rbx&7=3,rm=r14&7=6 -> 11_011_110 = 0xDE
    let mut e = Encoder::new();
    e.cmp_r64_r64(Reg::R14, Reg::RBX);
    assert_eq!(e.finish(), vec![0x49, 0x39, 0xDE]);
}

#[test]
fn add_r64_r64_known_bytes() {
    // add r14, rbx -- same register/REX/ModRM pattern as cmp, opcode 01 instead of 39.
    let mut e = Encoder::new();
    e.add_r64_r64(Reg::R14, Reg::RBX);
    assert_eq!(e.finish(), vec![0x49, 0x01, 0xDE]);
}

#[test]
fn sub_r64_r64_known_bytes() {
    // sub r14, rbx -- same pattern, opcode 29.
    let mut e = Encoder::new();
    e.sub_r64_r64(Reg::R14, Reg::RBX);
    assert_eq!(e.finish(), vec![0x49, 0x29, 0xDE]);
}

#[test]
fn imul_r64_r64_known_bytes() {
    // imul r14, rbx -- REX.W=1,R=1(r14 is reg/ext),B=0(rbx not ext) = 0100_1100 = 0x4C;
    // opcode 0F AF; modrm mod=11,reg=r14&7=6,rm=rbx&7=3 -> 11_110_011 = 0xF3
    let mut e = Encoder::new();
    e.imul_r64_r64(Reg::R14, Reg::RBX);
    assert_eq!(e.finish(), vec![0x4C, 0x0F, 0xAF, 0xF3]);
}

#[test]
fn imul_r64_imm32_known_bytes() {
    // imul rax, rax, 12 -- REX.W=1, 69 /r id. modrm mod=11,reg=0(rax),rm=0(rax) = 0xC0.
    let mut e = Encoder::new();
    e.imul_r64_imm32(Reg::RAX, 12);
    assert_eq!(e.finish(), vec![0x48, 0x69, 0xC0, 0x0C, 0x00, 0x00, 0x00]);
}

#[test]
fn load_r64_disp32_known_bytes() {
    // mov rax, [r15 + 200] -- REX.W=1,R=0(rax),B=1(r15) = 0x49; 8B; mod=10,reg=rax&7=0,rm=r15&7=7
    // = 10_000_111 = 0x87; disp32 = 200 = 0xC8 0x00 0x00 0x00.
    let mut e = Encoder::new();
    e.load_r64_disp32(Reg::RAX, Reg::R15, 200);
    assert_eq!(e.finish(), vec![0x49, 0x8B, 0x87, 0xC8, 0x00, 0x00, 0x00]);
}

#[test]
fn store_r64_disp32_known_bytes() {
    // mov [r15 + 200], rax -- same as load but opcode 89.
    let mut e = Encoder::new();
    e.store_r64_disp32(Reg::R15, 200, Reg::RAX);
    assert_eq!(e.finish(), vec![0x49, 0x89, 0x87, 0xC8, 0x00, 0x00, 0x00]);
}

#[test]
fn call_indirect_known_bytes() {
    let mut e = Encoder::new();
    e.call_r64(Reg::RAX);
    assert_eq!(e.finish(), vec![0xFF, 0xD0]);
}

#[test]
fn test_al_al_known_bytes() {
    let mut e = Encoder::new();
    e.test_al_al();
    assert_eq!(e.finish(), vec![0x84, 0xC0]);
}

#[test]
fn not_r64_known_bytes() {
    let mut e = Encoder::new();
    e.not_r64(Reg::R14);
    // REX.W=1,B=1 (r14 extended) = 0100_1001 = 0x49; opcode F7; modrm mod=11,reg=2(/2),rm=r14&7=6 -> 11_010_110 = 0xD6
    assert_eq!(e.finish(), vec![0x49, 0xF7, 0xD6]);
}

#[test]
fn backward_jz_lands_on_the_placed_label() {
    // top: xor r14,r14 (3-byte filler, just to give the jump real distance) ; jz top
    let mut e = Encoder::new();
    let top = e.label();
    e.place(top);
    e.xor_r64_self(Reg::R14);
    e.jz(top);
    let bytes = e.finish();
    // jz opcode starts at offset 3 (after the 3-byte xor); rel32 is at bytes[5..9].
    let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
    // end of the jz instruction is offset 9; target (0) - end (9) = -9.
    assert_eq!(rel, -9);
}

#[test]
fn backward_jnz_lands_on_the_placed_label() {
    let mut e = Encoder::new();
    let top = e.label();
    e.place(top);
    e.xor_r64_self(Reg::R14);
    e.jnz(top);
    let bytes = e.finish();
    assert_eq!(bytes[3], 0x0F);
    assert_eq!(bytes[4], 0x85);
    let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
    assert_eq!(rel, -9);
}

#[test]
fn backward_ja_lands_on_the_placed_label() {
    let mut e = Encoder::new();
    let top = e.label();
    e.place(top);
    e.xor_r64_self(Reg::R14);
    e.ja(top);
    let bytes = e.finish();
    assert_eq!(bytes[3], 0x0F);
    assert_eq!(bytes[4], 0x87);
    let rel = i32::from_le_bytes(bytes[5..9].try_into().unwrap());
    assert_eq!(rel, -9);
}

#[test]
fn ja_executes_only_when_unsigned_above() {
    // A real end-to-end check of the condition (not just byte shape): emit
    // `fn() -> i64 { if 5u64 > 3u64 { 1 } else { 0 } }` using cmp_r64_r64 + ja, run it, and
    // confirm the taken branch matches the actual x86 CF=0&&ZF=0 semantics, not just the byte
    // pattern.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    e.mov_r64_imm64(Reg::RAX, 5);
    e.mov_r64_imm64(Reg::RCX, 3);
    e.cmp_r64_r64(Reg::RAX, Reg::RCX); // 5 - 3 > 0
    let above = e.label();
    e.ja(above);
    e.mov_r64_imm64(Reg::RAX, 0);
    let end = e.label();
    e.jmp(end);
    e.place(above);
    e.mov_r64_imm64(Reg::RAX, 1);
    e.place(end);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(), 1);
}

#[test]
fn ja_correctly_treats_u64_max_as_unsigned_largest_not_negative() {
    // The exact bug this primitive exists to avoid: cap_clocks == u64::MAX (the "no cap"
    // sentinel some callers pass) must compare as the LARGEST u64, not as -1. A signed `jg`
    // would take the branch here (13 > -1 signed); `ja` must NOT, since 13 is not above
    // u64::MAX unsigned.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    e.mov_r64_imm64(Reg::RAX, 13);
    e.mov_r64_imm64(Reg::RCX, u64::MAX);
    e.cmp_r64_r64(Reg::RAX, Reg::RCX); // 13 vs u64::MAX, unsigned: 13 is NOT above
    let above = e.label();
    e.ja(above);
    e.mov_r64_imm64(Reg::RAX, 0); // not-above path: expected outcome
    let end = e.label();
    e.jmp(end);
    e.place(above);
    e.mov_r64_imm64(Reg::RAX, 1);
    e.place(end);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn() -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(), 0);
}

#[test]
fn forward_jmp_patches_to_a_later_label() {
    let mut e = Encoder::new();
    let exit = e.label();
    e.jmp(exit); // E9 at offset 0, 5 bytes total
    e.xor_r64_self(Reg::R14); // 3 bytes of filler the jump must skip
    e.place(exit);
    let bytes = e.finish();
    let rel = i32::from_le_bytes(bytes[1..5].try_into().unwrap());
    // end of the jmp instruction is offset 5; target (8) - end (5) = 3.
    assert_eq!(rel, 3);
}

#[test]
fn forward_jnz_patches_to_a_later_label() {
    let mut e = Encoder::new();
    let exit = e.label();
    e.jnz(exit); // 0F 85 at offset 0, 6 bytes total
    e.xor_r64_self(Reg::R14); // 3 bytes of filler
    e.place(exit);
    let bytes = e.finish();
    let rel = i32::from_le_bytes(bytes[2..6].try_into().unwrap());
    // end of the jnz instruction is offset 6; target (9) - end (6) = 3.
    assert_eq!(rel, 3);
}

#[test]
#[should_panic(expected = "a jcc/jmp target label was never placed")]
fn finish_panics_on_an_unresolved_forward_label() {
    let mut e = Encoder::new();
    let exit = e.label();
    e.jmp(exit); // forward reference, queued as a patch
    // `exit` is never `place`d -- `finish` must panic resolving the queued patch.
    let _ = e.finish();
}

#[test]
#[should_panic(expected = "label placed twice")]
fn place_panics_if_the_same_label_is_placed_twice() {
    let mut e = Encoder::new();
    let here = e.label();
    e.place(here);
    e.place(here);
}

#[test]
fn executes_an_emitted_increment_function() {
    // A real end-to-end check that the encoder's bytes actually run: emit
    // `fn(x: i64) -> i64 { x + 1 }` for whichever ABI the host uses, via mov_r64_r64 from the
    // arg register into rax then add_r64_imm32 and ret.
    use super::super::exec_mem::ExecutableBuffer;
    let mut e = Encoder::new();
    #[cfg(windows)]
    e.mov_r64_r64(Reg::RAX, Reg::RCX); // win64 arg0 = RCX
    #[cfg(not(windows))]
    e.mov_r64_r64(Reg::RAX, Reg::RDI); // sysv64 arg0 = RDI
    e.add_r64_imm32(Reg::RAX, 1);
    e.ret();
    let bytes = e.finish();
    let buf = ExecutableBuffer::new(&bytes).expect("alloc must succeed on a supported host");
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
    assert_eq!(f(41), 42);
}
