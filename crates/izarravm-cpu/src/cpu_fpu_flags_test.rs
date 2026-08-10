// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// ---- Task A13: x87 FPU (0xD8-0xDF) + WAIT (0x9B) decode/execute split ----

struct FpuGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
    fpu_control: u16,
    fpu_status: u16,
    fpu_tag: u16,
    /// The architectural stack ST(0)..ST(7), each f64 captured as raw bits (NaN-stable).
    st: [u64; 8],
}

/// Seed for the x87 FPU golden battery. Real mode, CS=DS=SS=0, 16-bit addressing. The FPU is
/// reset (FINIT state) and then ST(1)=1.25, ST(0)=3.5 are pushed so the stack ops (FADD ST0,ST1;
/// FXCH; FST; FNSTSW; FCOM; ...) have stable, distinct inputs; TOP therefore starts at 6. A
/// non-default control word (0x027f, the FINIT default) and a status condition are left as the
/// push set them. GPRs are a fixed pattern (AX..DI) so the FNSTSW-AX / integer-flag forms have an
/// observable before/after.
fn fpu_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1111);
    cpu.write_reg16(Reg16::Cx, 0x2222);
    cpu.write_reg16(Reg16::Dx, 0x3333);
    cpu.write_reg16(Reg16::Bx, 0x4444);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02;
    cpu.fpu.finit();
    cpu.fpu.push(1.25); // ST(1)
    cpu.fpu.push(3.5); // ST(0)
}

/// Plant the instruction bytes plus the float/int scratch the memory forms read. A 4-byte real
/// 2.0 at [0x100], an 8-byte real 1.5 at [0x108], a 4-byte int 7 at [0x110], a 2-byte int 9 at
/// [0x118], and a 16-bit control word 0x037f at [0x120] (for FLDCW). The store forms write into
/// the free area at [0x130] onward.
fn fpu_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x100..0x104].copy_from_slice(&2.0f32.to_le_bytes());
    mem[0x108..0x110].copy_from_slice(&1.5f64.to_le_bytes());
    mem[0x110..0x114].copy_from_slice(&7i32.to_le_bytes());
    mem[0x118..0x11a].copy_from_slice(&9i16.to_le_bytes());
    mem[0x120..0x122].copy_from_slice(&0x037fu16.to_le_bytes());
}

fn assert_fpu_state(cpu: &CpuGsw, g: &FpuGolden) {
    assert_eq!(cpu.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
    assert_eq!(cpu.eflags(), g.eflags, "eflags mismatch for {}", g.name);
    assert_eq!(cpu.registers.eip, g.eip, "eip mismatch for {}", g.name);
    assert_eq!(
        cpu.fpu.control, g.fpu_control,
        "fpu control mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.fpu.status, g.fpu_status,
        "fpu status mismatch for {}",
        g.name
    );
    assert_eq!(cpu.fpu.tag, g.fpu_tag, "fpu tag mismatch for {}", g.name);
    let st: [u64; 8] = std::array::from_fn(|i| cpu.fpu.get(i as u8).to_bits());
    assert_eq!(st, g.st, "fpu stack ST(0)..ST(7) mismatch for {}", g.name);
}

/// The x87 FPU differential battery (task A13). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy` -> `dispatch_opcode` -> `execute_fpu`) via `regen_fpu_goldens`
/// (parent commit 0b928034) WHILE the fused 0xD8-0xDF / 0x9B arms still existed. Never edit by
/// hand — re-run the regen from the pre-split commit. Covers a representative set: a memory load
/// (FLD m32), a memory store (FST m32), an FPU stack op (FADD ST0,ST1 and FXCH), the control word
/// (FLDCW / FNSTCW), the status word (FNSTSW AX and FNSTSW m16), a few arithmetic / compare ops,
/// an integer-operand memory form (FIADD m32), and WAIT/FWAIT (0x9B).
fn fpu_golden_cases() -> &'static [FpuGolden] {
    // Captured verbatim from the fused reference at parent 0b928034 via `regen_fpu_goldens`
    // (run in a throwaway worktree). Never edit by hand.
    &[
        FpuGolden {
            name: "fwait (9b)",
            code: &[155],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fld m32 [0x100] (d9 06 00 01)",
            code: &[217, 6, 0, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x2800,
            fpu_tag: 0x3ff,
            st: [
                0x4000000000000000,
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fst m32 [0x130] (d9 16 30 01)",
            code: &[217, 22, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(306, 96), (307, 64)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fstp m32 [0x130] (d9 1e 30 01)",
            code: &[217, 30, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(306, 96), (307, 64)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3800,
            fpu_tag: 0x3fff,
            st: [
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x400c000000000000,
            ],
        },
        FpuGolden {
            name: "fadd st0,st1 (d8 c1)",
            code: &[216, 193],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4013000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fmul st0,st1 (d8 c9)",
            code: &[216, 201],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4011800000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fcom st1 (d8 d1)",
            code: &[216, 209],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fxch st1 (d9 c9)",
            code: &[217, 201],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x3ff4000000000000,
                0x400c000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fadd m64 [0x108] (dc 06 08 01)",
            code: &[220, 6, 8, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4014000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fiadd m32 [0x110] (da 06 10 01)",
            code: &[218, 6, 16, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4025000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fldcw [0x120] (d9 2e 20 01)",
            code: &[217, 46, 32, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstcw [0x130] (d9 3e 30 01)",
            code: &[217, 62, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(304, 127), (305, 3)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstsw ax (df e0)",
            code: &[223, 224],
            gpr: [12288, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstsw m16 [0x130] (dd 3e 30 01)",
            code: &[221, 62, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(305, 48)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
    ]
}

#[test]
fn fpu_split_matches_golden_across_ops() {
    // The x87 FPU opcodes (0xD8-0xDF) and WAIT (0x9B) are converted to the decode/execute split,
    // so their fused arms are deleted and they can no longer be diffed against a fused executor
    // in-tree. Run each case through the split (`exec_one_split`) and assert the architectural
    // end-state — GPRs, eflags, the FPU control/status/tag words, and the architectural stack
    // ST(0)..ST(7) — against goldens captured from the pre-split fused path (parent 0b928034)
    // via `regen_fpu_goldens`. eip + fetch prove decode consumed and charged every byte (opcode +
    // ModRM + displacement) exactly once; the memory deltas prove the store path.
    for g in fpu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        fpu_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        fpu_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_fpu_state(&split, g);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

#[test]
fn fist_honors_rounding_control() {
    // FISTP m32 [0x130] (DB 1E 30 01) under all four RC modes. DJGPP-compiled code
    // (Quake) flips RC to chop around every C (int) cast; 80387 PRM table 15-2.
    let cases: &[(u16, f64, u32)] = &[
        (0b00, 2.5, 2), // nearest-even
        (0b01, 2.5, 2), // toward -inf
        (0b10, 2.5, 3), // toward +inf
        (0b11, 2.5, 2), // chop
        (0b00, -1.5, -2i32 as u32),
        (0b01, -1.5, -2i32 as u32),
        (0b10, -1.5, -1i32 as u32),
        (0b11, -1.5, -1i32 as u32),
    ];
    for &(rc, input, expected) in cases {
        let mut mem = vec![0u8; 0x200];
        mem[..4].copy_from_slice(&[0xdb, 0x1e, 0x30, 0x01]);
        let mut cpu = CpuGsw::default();
        fpu_seed(&mut cpu);
        cpu.fpu.control = 0x037f | (rc << 10);
        cpu.fpu.push(input);
        let mut bus = TestBus::with_memory(mem);
        exec_one_split(&mut cpu, &mut bus).unwrap();
        let got = u32::from_le_bytes(bus.memory[0x130..0x134].try_into().unwrap());
        assert_eq!(got, expected, "FISTP m32 of {input} with RC={rc:02b}");
        assert_eq!(
            cpu.fpu.status & 0x01,
            0,
            "no IE for the in-range FISTP of {input}"
        );
    }
}

#[test]
fn fist_overflow_stores_integer_indefinite_and_raises_ie() {
    // Out-of-range (and NaN) FIST stores the integer indefinite for the width and
    // raises IE (masked #IA response), rather than Rust's saturating cast.
    let m16: &[u8] = &[0xdf, 0x1e, 0x30, 0x01]; // FISTP m16
    let m32: &[u8] = &[0xdb, 0x1e, 0x30, 0x01]; // FISTP m32
    let m64: &[u8] = &[0xdf, 0x3e, 0x30, 0x01]; // FISTP m64
    let cases: &[(&[u8], f64, Vec<u8>)] = &[
        (m16, 40000.0, 0x8000u16.to_le_bytes().to_vec()),
        (m16, -40000.0, 0x8000u16.to_le_bytes().to_vec()),
        (m32, 3.0e9, 0x8000_0000u32.to_le_bytes().to_vec()),
        (m64, 1.0e19, 0x8000_0000_0000_0000u64.to_le_bytes().to_vec()),
        (m32, f64::NAN, 0x8000_0000u32.to_le_bytes().to_vec()),
    ];
    for (code, input, expected) in cases {
        let mut mem = vec![0u8; 0x200];
        mem[..code.len()].copy_from_slice(code);
        let mut cpu = CpuGsw::default();
        fpu_seed(&mut cpu);
        cpu.fpu.push(*input);
        let mut bus = TestBus::with_memory(mem);
        exec_one_split(&mut cpu, &mut bus).unwrap();
        let got = &bus.memory[0x130..0x130 + expected.len()];
        assert_eq!(got, expected, "indefinite for FISTP of {input}");
        assert_ne!(cpu.fpu.status & 0x01, 0, "IE raised for FISTP of {input}");
    }
}

#[test]
fn frndint_honors_rounding_control() {
    for (rc, expected) in [(0u16, -2.0), (1, -2.0), (2, -1.0), (3, -1.0)] {
        let mut cpu = CpuGsw::default();
        fpu_seed(&mut cpu);
        cpu.fpu.control = 0x037f | (rc << 10);
        cpu.fpu.push(-1.5);
        let mut bus = TestBus::with_memory({
            let mut mem = vec![0u8; 0x200];
            mem[..2].copy_from_slice(&[0xd9, 0xfc]); // FRNDINT
            mem
        });
        exec_one_split(&mut cpu, &mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), expected, "FRNDINT of -1.5 with RC={rc:02b}");
    }
}

/// Run one instruction against a fresh FPU seeded with the given stack (last
/// element becomes ST(0)) and return the CPU for state assertions. The x87
/// value-accuracy battery below uses manual-cited inputs per family; the
/// differential goldens above pin encodings, these pin VALUES.
fn fpu_exec(code: &[u8], stack: &[f64]) -> (CpuGsw, TestBus) {
    let mut mem = vec![0u8; 0x200];
    mem[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.eflags = 0x02;
    cpu.fpu.finit();
    for &v in stack {
        cpu.fpu.push(v);
    }
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    (cpu, bus)
}

/// Condition codes C3/C2/C1/C0 from the status word, as a tuple.
fn cc(cpu: &CpuGsw) -> (bool, bool, bool, bool) {
    let s = cpu.fpu.status;
    (
        s & (1 << 14) != 0,
        s & (1 << 10) != 0,
        s & (1 << 9) != 0,
        s & (1 << 8) != 0,
    )
}

#[test]
fn fld_fstp_m80_round_trips_exact_values() {
    // FLD m80 [0x100]: 1.5 in extended = sign 0, exponent 16383, mantissa
    // 0xC000000000000000 (explicit integer bit + 0.5), 80387 PRM data formats.
    let mut mem = vec![0u8; 0x200];
    mem[..4].copy_from_slice(&[0xdb, 0x2e, 0x00, 0x01]); // FLD tbyte [0x100]
    mem[0x100..0x108].copy_from_slice(&0xC000_0000_0000_0000u64.to_le_bytes());
    mem[0x108..0x10a].copy_from_slice(&0x3FFFu16.to_le_bytes());
    let mut cpu = CpuGsw::default();
    fpu_seed(&mut cpu);
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.5, "FLD m80 of extended 1.5");

    // FSTP m80 [0x130] of -2.0: sign 1, exponent 16384, integer-bit-only mantissa.
    let (_, bus) = fpu_exec(&[0xdb, 0x3e, 0x30, 0x01], &[-2.0]);
    assert_eq!(
        bus.memory[0x130..0x138],
        0x8000_0000_0000_0000u64.to_le_bytes(),
        "FSTP m80 mantissa of -2.0"
    );
    assert_eq!(
        bus.memory[0x138..0x13a],
        0xC000u16.to_le_bytes(),
        "FSTP m80 sign+exponent of -2.0"
    );
}

#[test]
fn faulting_push_leaves_sp_unchanged() {
    // A push whose stack write faults must leave (E)SP at its
    // pre-instruction value so the restart after the handler re-executes
    // cleanly (386 PRM fault-restart semantics). CWSDPMI grows the DJGPP
    // stack by committing the page in its #PF handler and retrying; a
    // committed-then-faulted ESP double-decrements on the retry.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x8000); // stack target beyond the test memory
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x50; // PUSH AX
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x8000,
        "SP unchanged after the faulting push"
    );
}

#[test]
fn fpatan_quadrants() {
    // FPATAN: ST1 = atan(ST1/ST0) with quadrant correction, then pop.
    // atan2(1, -1) = 3pi/4 (80387 PRM: operand signs select the quadrant).
    let (cpu, _) = fpu_exec(&[0xd9, 0xf3], &[1.0, -1.0]); // ST1=1 (y), ST0=-1 (x)
    let want = 3.0 * std::f64::consts::FRAC_PI_4;
    assert!(
        (cpu.fpu.get(0) - want).abs() < 1e-15,
        "FPATAN(y=1, x=-1) = 3pi/4, got {}",
        cpu.fpu.get(0)
    );
}

#[test]
fn fprem_positive_quotient_low_bits_land_in_c0_c3_c1() {
    // FPREM: 17 mod 5 = 2 with quotient 3; C0/C3/C1 = quotient bits 2/1/0 =
    // 0/1/1, C2 = 0 (reduction complete). 80387 PRM FPREM description.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf8], &[5.0, 17.0]); // ST1=5, ST0=17
    assert_eq!(cpu.fpu.get(0), 2.0, "17 rem 5");
    let (c3, c2, c1, c0) = cc(&cpu);
    assert!(!c2, "C2 clear: reduction complete");
    assert!(!c0 && c3 && c1, "quotient 3 = 0b011 in C0/C3/C1");
}

#[test]
fn fprem1_uses_round_to_nearest_quotient() {
    // FPREM1 separates from FPREM at 8 mod 5: the IEEE nearest quotient of
    // 8/5 = 1.6 is 2, remainder -2 (FPREM's truncated quotient 1 leaves +3).
    let (cpu, _) = fpu_exec(&[0xd9, 0xf5], &[5.0, 8.0]);
    assert_eq!(cpu.fpu.get(0), -2.0, "FPREM1 8 rem 5 (nearest quotient 2)");
    let (_, c2, _, _) = cc(&cpu);
    assert!(!c2);
}

#[test]
fn fxtract_splits_exponent_and_significand() {
    // FXTRACT on 6.0: exponent 2 replaces ST(0), significand 1.5 is pushed.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf4], &[6.0]);
    assert_eq!(cpu.fpu.get(0), 1.5, "significand of 6.0");
    assert_eq!(cpu.fpu.get(1), 2.0, "unbiased exponent of 6.0");
}

#[test]
fn fscale_truncates_the_scale_toward_zero() {
    // FSCALE: ST0 = ST0 * 2^trunc(ST1); the fractional and negative scales
    // truncate toward zero (the integer case is covered by
    // `fscale_scales_by_power_of_two`). trunc(2.5) = 2 -> 12; trunc(-1.5) =
    // -1 -> 1.5. 80387 PRM FSCALE.
    let (cpu, _) = fpu_exec(&[0xd9, 0xfd], &[2.5, 3.0]);
    assert_eq!(cpu.fpu.get(0), 12.0, "3.0 scaled by trunc(2.5)");
    let (cpu, _) = fpu_exec(&[0xd9, 0xfd], &[-1.5, 3.0]);
    assert_eq!(cpu.fpu.get(0), 1.5, "3.0 scaled by trunc(-1.5)");
}

#[test]
fn fxam_classifies_and_signs() {
    // FXAM: C3/C2/C0 classify ST(0), C1 = sign. 80387 PRM table: zero = C3,
    // NaN = C0, infinity = C2+C0, normal = C2, empty = C3+C0.
    let cases: &[(f64, (bool, bool, bool))] = &[
        (0.0, (true, false, false)),
        (f64::NAN, (false, false, true)),
        (f64::INFINITY, (false, true, true)),
        (1.0, (false, true, false)),
    ];
    for &(v, (want_c3, want_c2, want_c0)) in cases {
        let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[v]);
        let (c3, c2, _, c0) = cc(&cpu);
        assert_eq!((c3, c2, c0), (want_c3, want_c2, want_c0), "FXAM of {v}");
    }
    let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[-1.0]);
    let (_, _, c1, _) = cc(&cpu);
    assert!(c1, "FXAM C1 = sign of -1.0");
    let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[]);
    let (c3, _, _, c0) = cc(&cpu);
    assert!(c3 && c0, "FXAM of an empty ST(0)");
}

#[test]
fn f2xm1_and_fyl2x_hit_exact_and_near_values() {
    // F2XM1 on 0.5: 2^0.5 - 1 = sqrt(2) - 1.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf0], &[0.5]);
    assert!(
        (cpu.fpu.get(0) - (std::f64::consts::SQRT_2 - 1.0)).abs() < 1e-15,
        "F2XM1(0.5)"
    );
    // FYL2X: ST1 * log2(ST0), pop. 3 * log2(8) = 9 exactly in f64.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf1], &[3.0, 8.0]); // ST1=3, ST0=8
    assert_eq!(cpu.fpu.get(0), 9.0, "FYL2X exact case");
    assert_eq!(cpu.fpu.top(), 7, "FYL2X popped once from a 2-deep stack");
}

#[test]
fn fsincos_pushes_cos_over_sin() {
    // FSINCOS on 0.0: ST(1) = sin = 0, ST(0) = cos = 1, C2 = 0.
    let (cpu, _) = fpu_exec(&[0xd9, 0xfb], &[0.0]);
    assert_eq!(cpu.fpu.get(0), 1.0, "cos(0)");
    assert_eq!(cpu.fpu.get(1), 0.0, "sin(0)");
    let (_, c2, _, _) = cc(&cpu);
    assert!(!c2, "C2 clear: argument in range");
}

#[test]
fn fcompp_compares_and_pops_both() {
    // FCOMPP (DE D9): compare ST(0) with ST(1), pop both. 2 < 3 -> C0 set.
    let (cpu, _) = fpu_exec(&[0xde, 0xd9], &[3.0, 2.0]); // ST1=3, ST0=2
    let (c3, _, _, c0) = cc(&cpu);
    assert!(c0 && !c3, "2 < 3 sets C0");
    assert_eq!(cpu.fpu.top(), 0, "both operands popped");
    assert!(cpu.fpu.is_empty(0), "stack empty after FCOMPP");
}

#[test]
fn fbld_fbstp_round_trip_packed_bcd() {
    // FBLD [0x100] of packed BCD 1234567; FBSTP writes the digits back with
    // the sign in bit 7 of byte 9. 80387 PRM packed-BCD format.
    let mut mem = vec![0u8; 0x200];
    mem[..4].copy_from_slice(&[0xdf, 0x26, 0x00, 0x01]); // FBLD [0x100]
    mem[0x100] = 0x67;
    mem[0x101] = 0x45;
    mem[0x102] = 0x23;
    mem[0x103] = 0x01;
    let mut cpu = CpuGsw::default();
    fpu_seed(&mut cpu);
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1234567.0, "FBLD 1234567");

    let (cpu, bus) = fpu_exec(&[0xdf, 0x36, 0x30, 0x01], &[-1234567.0]); // FBSTP [0x130]
    assert_eq!(
        &bus.memory[0x130..0x134],
        &[0x67, 0x45, 0x23, 0x01],
        "FBSTP digits"
    );
    assert_eq!(bus.memory[0x139], 0x80, "FBSTP sign byte for a negative");
    assert_eq!(cpu.fpu.top(), 0, "FBSTP popped");
}

#[test]
fn faulting_push_leaves_esp_unchanged_on_a_32bit_stack() {
    // The SS.B=1 arm - the one a DPMI flat 32-bit stack (CWSDPMI/DJGPP)
    // actually exercises: the full ESP must stay at its pre-instruction
    // value when the push's write faults.
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x10,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0001_8000); // beyond the 0x200-byte test memory
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x50; // PUSH (E)AX
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.registers.esp(),
        0x0001_8000,
        "ESP unchanged after the faulting push on a 32-bit stack"
    );
}

#[test]
fn faulting_pusha_restores_sp_past_committed_pushes() {
    // PUSHA: the first two pushes land, the third faults; (E)SP must come
    // back to the pre-instruction value (386 PRM: PUSHA restores ESP so
    // the whole instruction restarts).
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x0004); // AX@2, CX@0 land; DX@0xfffe faults
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x60; // PUSHA
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0004,
        "SP restored after the faulting PUSHA"
    );
}

/// Regenerate `fpu_golden_cases` from the fused reference. Ignored by default. Run WHILE the
/// x87 fused arms still exist (parent commit 0b928034):
///   git worktree add ../regen-a13 0b928034
///   cd ../regen-a13
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_fpu_goldens -- --ignored --nocapture
/// then paste the output over `fpu_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_fpu_goldens() {
    for g in fpu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        fpu_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        fpu_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        let st: [u64; 8] = std::array::from_fn(|i| fused.fpu.get(i as u8).to_bits());
        println!(
            "            FpuGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {}, fpu_control: {:#x}, fpu_status: {:#x}, fpu_tag: {:#x}, st: [{} ] }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
            fused.fpu.control,
            fused.fpu.status,
            fused.fpu.tag,
            st.iter()
                .map(|b| format!(" {b:#018x},"))
                .collect::<String>(),
        );
    }
}

// ── Task A14: the heterogeneous one-off golden battery ─────────────────────────────────────────

/// One golden end-state for a Misc case (task A14). Captures the architectural register file
/// (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, the (offset,value) memory writes, the instruction-
/// fetch cycle count. Port reads via TestBus always return 0, so the IN/OUT-derived register/memory
/// values reflect the read-zero behaviour; the port traffic itself is asserted separately by the
/// dedicated INS/OUTS tests.
struct MiscGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the Misc golden battery: a fixed register file giving BCD/IMUL/TEST/XLAT stable
/// inputs. AL=0x29, AH=0x05 (so DAA/AAA/AAM/AAD/TEST exercise the adjust/flag paths); CF/AF preset
/// so DAA/DAS see an incoming carry; BX=0x10 (XLAT base); CX/DX/SI/DI/BP fixed. EDX:EAX and ECX:EBX
/// are also given known 32-bit halves for CMPXCHG8B (set after this via the high words below).
fn misc_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_0529); // AL=0x29, AH=0x05
    cpu.registers.set_ecx(0x0000_0304);
    cpu.registers.set_edx(0x0000_0506);
    cpu.registers.set_ebx(0x0000_0010);
    cpu.registers.set_esi(0x0000_0008);
    cpu.registers.set_edi(0x0000_0018);
    cpu.registers.set_ebp(0x0000_0010);
    cpu.registers.eflags = 0x13; // CF=1, AF=1 (bit 4) on top of the always-1 bit 1
    // Seed a non-trivial x87 tag word. No surviving Misc opcode touches the FPU, so the tag is
    // an INVARIANT across the whole battery rather than a per-case end-state -- but it is only
    // worth pinning if the seeded value is distinctive. Left at the FINIT default the tag would
    // be 0xffff, and a stray `finit()` inside a Misc executor would reproduce it exactly and go
    // unnoticed. Three pushes covering all three non-empty tag encodings (0b01 zero, 0b10
    // special, 0b00 valid) give a value no reset or clear can forge.
    cpu.fpu.finit();
    cpu.fpu.push(0.0); // tag 0b01, physical 7
    cpu.fpu.push(f64::INFINITY); // tag 0b10, physical 6
    cpu.fpu.push(1.25); // tag 0b00, physical 5
}

/// The x87 tag word `misc_seed` leaves behind, asserted unchanged after every Misc case. See the
/// seed for why this is one shared constant instead of a per-case `MiscGolden` column: the Misc
/// block is BCD/IMUL/TEST/XLAT/SALC/HLT/CPUID/RDTSC/CMPXCHG8B, none of which reads or writes x87
/// state, so a per-case column would be the same literal 28 times. Pinning it once still catches
/// the regression that matters -- a Misc executor that disturbs the FPU -- and additionally
/// catches a broken tag computation in the seed path itself.
/// Derivation: FINIT leaves 0xffff (all eight empty) and TOP 0; the three pushes land on physical
/// 7, 6 and 5 and rewrite those tag fields to 0b01, 0b10 and 0b00, clearing bits 15-10 to
/// 0b011000 and leaving physical 4-0 empty.
const MISC_SEED_X87_TAG: u16 = 0x63ff;

/// Seed memory for the Misc battery: plant the XLAT lookup table byte at [BX+AL]=[0x39] and an
/// m64 for CMPXCHG8B at [0x40].
fn misc_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x39] = 0xab; // XLAT: [DS:BX+AL] with BX=0x10, AL=0x29 -> 0x39
    mem[0x40..0x48].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes()); // CMPXCHG8B m64
}

/// The heterogeneous one-off differential battery (task A14). Captured from the PRIOR fused
/// reference (`execute_instruction_legacy`) via `regen_misc_goldens` at parent commit f1d65e0f
/// WHILE the fused arms (single-byte 0x27/0x2f/0x37/0x3f/0x69/0x6b/0x6c-0x6f/0xa8/0xa9/0xd4/0xd5/
/// 0xd6/0xd7/0xf4 and the 0F CMPXCHG8B/CPUID/RDTSC/...) still existed. Never edit by hand —
/// re-run the regen from the pre-split commit. Covers: DAA/DAS/AAA/AAS (BCD flag effects),
/// AAM/AAD (incl. the imm8 base), TEST AL/AX,imm (flags only), IMUL r,r/m,imm8/imm16 (OF/CF set),
/// SALC, XLAT (memory read), HLT, CPUID, RDTSC, and CMPXCHG8B. (The MMX cases the battery once
/// carried went with the MMX block: the GSW-586 has no SIMD extension, so those encodings are
/// invalid and their #UD is asserted in `cpu_persona_system_test.rs` instead.)
fn misc_golden_cases() -> &'static [MiscGolden] {
    // Captured verbatim from the fused reference at parent f1d65e0f via `regen_misc_goldens`
    // (run in a throwaway worktree). Never edit by hand.
    MISC_GOLDEN_CASES
}

/// The captured Misc golden literals. The `code`/`name` are authored; the remaining fields are
/// the fused reference's end-state, pasted verbatim from `regen_misc_goldens` (parent f1d65e0f).
/// gpr/code are the regen's printed (decimal) literals; do not hand-edit.
const MISC_GOLDEN_CASES: &[MiscGolden] = &[
    MiscGolden {
        name: "daa (27)",
        code: &[39],
        gpr: [1423, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x93,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "das (2f)",
        code: &[47],
        gpr: [1475, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x97,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "aaa (37)",
        code: &[55],
        gpr: [1551, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "aas (3f)",
        code: &[63],
        gpr: [1027, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "aam (d4 0a)",
        code: &[212, 10],
        gpr: [1025, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
    },
    MiscGolden {
        name: "aad (d5 0a)",
        code: &[213, 10],
        gpr: [91, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
    },
    MiscGolden {
        name: "test al,imm8 (a8 0f)",
        code: &[168, 15],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x16,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
    },
    MiscGolden {
        name: "test ax,imm16 (a9 ff 00)",
        code: &[169, 255, 0],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x12,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
    },
    MiscGolden {
        name: "imul ax,bx,imm8 (6b c3 02)",
        code: &[107, 195, 2],
        gpr: [32, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x12,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
    },
    MiscGolden {
        name: "imul ax,bx,imm16 (69 c3 00 40)",
        code: &[105, 195, 0, 64],
        gpr: [0, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x813,
        eip: 0x4,
        deltas: &[],
        fetch: 5,
    },
    MiscGolden {
        name: "salc (d6)",
        code: &[214],
        gpr: [1535, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "xlat (d7)",
        code: &[215],
        gpr: [1451, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
    },
    MiscGolden {
        name: "rdtsc (0f 31)",
        code: &[15, 49],
        gpr: [0, 772, 0, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
    },
    MiscGolden {
        name: "cmpxchg8b [0x40] (0f c7 0e 40 00)",
        code: &[15, 199, 14, 64, 0],
        gpr: [84281096, 772, 16909060, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x5,
        deltas: &[],
        fetch: 6,
    },
];

#[test]
fn misc_split_matches_golden_across_ops() {
    // The Misc one-off opcodes are converted to the decode/execute split, so their fused arms
    // are deleted and they can no longer be diffed against a fused executor in-tree. Run each
    // case through the split (`exec_one_split`) and assert the architectural end-state — GPRs,
    // eflags, the x87 tag word and the memory writes — against goldens captured from the
    // pre-split fused path (parent f1d65e0f) via `regen_misc_goldens`. eip + fetch prove decode
    // consumed and charged every byte (opcode + ModRM + displacement + immediate) exactly once.
    for g in misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = CpuGsw::default();
        misc_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        assert_eq!(
            split.fpu.tag, MISC_SEED_X87_TAG,
            "x87 tag word disturbed by {}",
            g.name
        );
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// AAM with a base of 0 is a divide error (#DE) — the only Misc op that faults on its operand,
/// so it is asserted here (through the split) rather than carried as a golden end-state.
/// (`aam_zero_divisor_is_divide_error` covers the same via `cycle`; this pins the split decode
/// path specifically: decode fetches the imm8 base, the executor raises #DE on base 0.)
#[test]
fn misc_aam_base_zero_is_divide_error() {
    let (mut cpu, memory) = real_mode_cpu(&[0xd4, 0x00], 0x20);
    let mut bus = TestBus::with_memory(memory);
    assert!(
        matches!(
            exec_one_split(&mut cpu, &mut bus),
            Err(InternalFault::Exception {
                vector: 0,
                error_code: None
            })
        ),
        "AAM base 0 must raise a deliverable #DE through the split"
    );
}

/// Regenerate `MISC_GOLDEN_CASES` from the fused reference. Ignored by default. Run WHILE the
/// fused one-off arms still exist (parent commit f1d65e0f):
///   git worktree add ../regen-a14 f1d65e0f
///   cd ../regen-a14
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_misc_goldens -- --ignored --nocapture
/// then paste the output over `MISC_GOLDEN_CASES` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_misc_goldens() {
    for g in misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = CpuGsw::default();
        misc_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "    MiscGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

#[test]
fn eager_flag_write_after_pending_is_correct() {
    // A pending ADD sets CF; a later CLC-like set_flag must clear CF while leaving the
    // pending-derived ZF intact, without forcing the rest of the lazy flags live.
    let mut cpu = CpuGsw::default();
    let r = cpu.alu_add_eager(0xff, 0x01, 0, BusWidth::Byte); // CF=1, ZF=1 (result 0x00)
    let lf = LazyFlags {
        a: 0xff,
        b: 0x01,
        result: r,
        width: BusWidth::Byte,
        op: LazyFlagOp::Add,
        cf_override: None,
    };
    let mut lazy = CpuGsw {
        pending_flags: PendingFlags::from_legacy(&lf),
        ..Default::default()
    };
    lazy.reset_perf_counters();
    lazy.set_flag(FLAG_CF, false); // CLC-like eager write
    assert!(!lazy.flag(FLAG_CF), "CF must be cleared by the eager write");
    assert!(
        lazy.flag(FLAG_ZF),
        "ZF from the pending descriptor must survive"
    );
    assert!(
        lazy.pending_flags.tag & (1u32 << 31) != 0,
        "single-CF writes should use the lazy CF override"
    );
    assert_eq!(
        lazy.perf.flag_materializations, 0,
        "CF override should not materialize lazy flags"
    );
}

#[test]
fn non_arithmetic_flag_write_after_pending_stays_lazy() {
    let mut lazy = CpuGsw::default();
    lazy.alu_sub(1, 1, 0, BusWidth::Byte); // pending ZF=1
    lazy.reset_perf_counters();

    lazy.set_flag(FLAG_DF, true);

    assert!(lazy.flag(FLAG_DF), "DF write must be visible");
    assert!(lazy.flag(FLAG_ZF), "pending arithmetic flags must survive");
    assert!(
        lazy.pending_flags.tag & (1u32 << 31) != 0,
        "non-arithmetic writes should not settle pending arithmetic flags"
    );
    assert_eq!(
        lazy.perf.flag_materializations, 0,
        "non-arithmetic writes should not materialize lazy flags"
    );
}

#[test]
fn lazy_flag_read_matches_eager_for_add_and_sub() {
    // arith_flag computed from a pending descriptor must equal the eager eflags bit for every
    // arithmetic flag, across widths and a spread of operand pairs (incl. carry/borrow/overflow/zero).
    let cases: &[(u32, u32, BusWidth)] = &[
        (0xff, 0x01, BusWidth::Byte),
        (0x7f, 0x01, BusWidth::Byte),
        (0x00, 0x00, BusWidth::Byte),
        (0x01, 0xff, BusWidth::Byte), // a < b: SUB borrow path sets CF=1
        (0x80, 0x80, BusWidth::Byte),
        (0xffff, 0x1, BusWidth::Word),
        (0x8000, 0x8000, BusWidth::Word),
        (0xffff_ffff, 0x1, BusWidth::Dword),
        (0x1234_5678, 0x8765_4321, BusWidth::Dword),
    ];
    for &(a, b, w) in cases {
        for is_sub in [false, true] {
            let mut eager = CpuGsw::default();
            let r = if is_sub {
                eager.alu_sub_eager(a, b, 0, w)
            } else {
                eager.alu_add_eager(a, b, 0, w)
            };
            let lf = LazyFlags {
                a: a & width_mask(w),
                b: b & width_mask(w),
                result: r,
                width: w,
                op: if is_sub {
                    LazyFlagOp::Sub
                } else {
                    LazyFlagOp::Add
                },
                cf_override: None,
            };
            let lazy = CpuGsw {
                pending_flags: PendingFlags::from_legacy(&lf),
                ..Default::default()
            };
            for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
                assert_eq!(
                    lazy.flag(f),
                    eager.flag(f),
                    "flag {f:#x} a={a:#x} b={b:#x} sub={is_sub} w={w:?}"
                );
            }
        }
    }
}

#[test]
fn alu_add_defers_and_reads_back_identically() {
    // alu_add (carry 0) must set a pending whose flag reads equal the eager path's eflags bit-for-bit.
    for &(a, b, w) in &[
        (0xff_u32, 0x01_u32, BusWidth::Byte),
        (0x1234_5678_u32, 0x8765_4321_u32, BusWidth::Dword),
    ] {
        let mut eager = CpuGsw::default();
        let er = eager.alu_add_eager(a, b, 0, w);
        let mut lazy = CpuGsw::default();
        let lr = lazy.alu_add(a, b, 0, w);
        assert_eq!(lr, er, "result");
        assert!(
            lazy.pending_flags.tag & (1u32 << 31) != 0,
            "carry-0 ADD must defer"
        );
        for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
            assert_eq!(lazy.flag(f), eager.flag(f), "flag {f:#x}");
        }
    }
}

#[test]
fn alu_sub_defers_and_reads_back_identically() {
    // alu_sub (borrow 0) must set a pending whose flag reads equal the eager path's eflags bit-for-bit.
    for &(a, b, w) in &[
        (0x01_u32, 0xff_u32, BusWidth::Byte),
        (0x1234_5678_u32, 0x8765_4321_u32, BusWidth::Dword),
    ] {
        let mut eager = CpuGsw::default();
        let er = eager.alu_sub_eager(a, b, 0, w);
        let mut lazy = CpuGsw::default();
        let lr = lazy.alu_sub(a, b, 0, w);
        assert_eq!(lr, er, "result");
        assert!(
            lazy.pending_flags.tag & (1u32 << 31) != 0,
            "borrow-0 SUB must defer"
        );
        for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
            assert_eq!(lazy.flag(f), eager.flag(f), "flag {f:#x}");
        }
    }
}

#[test]
fn whole_eflags_read_materializes_pending() {
    // Reading the whole eflags word (e.g. via eflags()) after a pending op must equal the eager result.
    let mut eager = CpuGsw::default();
    let r = eager.alu_add_eager(0x80, 0x80, 0, BusWidth::Byte); // CF=1, OF=1, ZF=1
    let lf = LazyFlags {
        a: 0x80,
        b: 0x80,
        result: r,
        width: BusWidth::Byte,
        op: LazyFlagOp::Add,
        cf_override: None,
    };
    let mut lazy = CpuGsw {
        pending_flags: PendingFlags::from_legacy(&lf),
        ..Default::default()
    };
    assert_eq!(
        lazy.eflags(),
        eager.registers.eflags,
        "materialized whole eflags must match eager"
    );
    lazy.materialize_flags();
    assert!(lazy.pending_flags.is_none());
    assert_eq!(lazy.registers.eflags, eager.registers.eflags);
}

#[test]
fn alu_logic_defers_flags_and_preserves_aux() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_AF | FLAG_CF | FLAG_OF, true);
    let result = cpu.alu(4, 0xf0, 0x0f, BusWidth::Byte);
    assert_eq!(result, 0);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "logic flags stay lazy"
    );
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF), "AF remains the previous undefined value");
    cpu.materialize_flags();
    assert_eq!(cpu.registers.eflags & (FLAG_CF | FLAG_OF), 0);
    assert_ne!(cpu.registers.eflags & FLAG_AF, 0);
}

#[test]
fn inc_dec_defers_flags_while_preserving_carry() {
    let mut cpu = CpuGsw::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.inc_dec(0xffff, false, BusWidth::Word);
    assert_eq!(result, 0);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "INC should not materialize just to keep CF"
    );
    assert!(cpu.flag(FLAG_CF), "INC preserves CF");
    assert!(cpu.flag(FLAG_ZF));

    cpu.set_flag(FLAG_CF, false);
    let result = cpu.inc_dec(0, true, BusWidth::Byte);
    assert_eq!(result, 0xff);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "DEC should stay lazy"
    );
    assert!(!cpu.flag(FLAG_CF), "DEC preserves CF");
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shift_after_pending_flags_matches_materialized_without_materializing() {
    for &(op, value, count) in &[
        (4, 0x4000, 1), // SHL defines OF
        (4, 0x0001, 2), // SHL preserves previous OF for multi-bit counts
        (5, 0x8001, 2), // SHR
        (7, 0x8001, 2), // SAR
    ] {
        let mut expected = CpuGsw::default();
        expected.alu_add(0x7f, 0x01, 0, BusWidth::Byte); // pending OF+AF
        expected.materialize_flags();
        let expected_result = expected.shift_rotate(op, value, count, BusWidth::Word);
        let expected_flags = expected.eflags();

        let mut lazy = CpuGsw::default();
        lazy.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
        lazy.reset_perf_counters();
        let lazy_result = lazy.shift_rotate(op, value, count, BusWidth::Word);

        assert_eq!(lazy_result, expected_result, "op={op} count={count}");
        assert_eq!(lazy.eflags(), expected_flags, "op={op} count={count}");
        assert_eq!(lazy.perf.flag_materializations, 0, "op={op} count={count}");
        assert!(lazy.pending_flags.is_none(), "op={op} count={count}");
    }

    let mut expected = CpuGsw::default();
    expected.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
    expected.materialize_flags();
    let expected_result = expected.double_shift(true, 0x0001, 0, 2, OperandSize::Word);
    let expected_flags = expected.eflags();

    let mut lazy = CpuGsw::default();
    lazy.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
    lazy.reset_perf_counters();
    let lazy_result = lazy.double_shift(true, 0x0001, 0, 2, OperandSize::Word);

    assert_eq!(lazy_result, expected_result);
    assert_eq!(lazy.eflags(), expected_flags);
    assert_eq!(lazy.perf.flag_materializations, 0);
    assert!(lazy.pending_flags.is_none());
}

#[test]
fn fp_timing_identity_does_not_change_fpu_clocks() {
    // FADD ST,ST(1) is opcode D8 C1 (register form: D8 /0, modrm=C1 → mod=3, reg=0, rm=1).
    // The FPU executor charges 20 raw clocks for a register-form arithmetic op
    // (fpu_reg_arith_st0 returns clocks(20)). With fp_timing==(1,1) the identity
    // scale_fp_clocks call must return 20 unchanged, so elapsed_clocks after one cycle
    // at I486 must equal elapsed_clocks at I586 — proving the FP factor is truly identity
    // and does not disturb the existing level_timing scaling.
    //
    // We also push 1.0 into ST0 and ST1 first so FADD does not trap on an empty stack;
    // that means we run three cycles total (FLD1; FLD1; FADD ST,ST(1)) and then measure.
    // But to isolate just the FADD clock charge, we record elapsed_clocks before and after
    // the FADD cycle at each level.
    let code: &[u8] = &[
        0xd9, 0xe8, // FLD1  → ST0 = 1.0
        0xd9, 0xe8, // FLD1  → push 1.0 again (ST0=1, ST1=1)
        0xd8, 0xc1, // FADD ST(0), ST(1)
    ];

    let fadd_elapsed = |mode: GswMode| -> u64 {
        let (mut cpu, memory) = real_mode_cpu(code, 0x20);
        cpu.set_mode(mode);
        let mut bus = TestBus::with_memory(memory);
        // Execute FLD1; FLD1 to load the stack.
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        // Snapshot before the FADD.
        let before = cpu.elapsed_clocks;
        cpu.cycle(&mut bus).unwrap();
        cpu.elapsed_clocks - before
    };

    let fadd_i486 = fadd_elapsed(GswMode::Gsw486);
    let fadd_i586 = fadd_elapsed(GswMode::Gsw586);

    // Both modes share level_timing (1,12); the per-class FP dial is identity at
    // I486 and Register-class x0.25 at I586 (P5 pairing/issue-rate honesty), so
    // the register FADD charge at 586 must be at most the 486 charge and both
    // must stay nonzero (the fractional carry may not round a cheap op to a
    // permanent zero).
    assert!(
        fadd_i586 <= fadd_i486,
        "per-class fp dial: register FADD at I586 ({fadd_i586}) must not exceed I486 ({fadd_i486})"
    );
    assert!(
        fadd_i486 > 0,
        "FADD must charge at least 1 scaled clock at I486 (got {fadd_i486})"
    );
}
