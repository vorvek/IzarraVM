// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Three test groups that came from the second recompiler (mkII) and had no twin in the Direct
//! tests. They run on Direct and do not use any mkII code.
//!
//! - `cap_cycle`: a native loop must stop at the same instruction as a stepping interpreter that
//!   applies the budget rule itself.
//! - `condition_matrix`: ADD, SUB and CMP with a memory source, followed by each of the 16 Jcc
//!   conditions, in three operand widths.
//! - `live_guards`: after a block is installed, one guest fact changes. Direct must leave the block
//!   or run it, and in both cases match the interpreter.
//!
//! Each group proves that native code ran. A differential test that never leaves the interpreter
//! passes every comparison, so each group asserts on the Direct counters as well.

use super::jit_direct::sixteen_bit::{arm_native_sixteen_bit, sixteen_bit_bus, warm_sixteen_bit};
use super::*;

/// A 16-bit protected-mode CPU: 16-bit code and stack segments with a 64 KB limit, flat data
/// segments, paging off.
fn pm16_flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    let mut cs = SegmentRegister::flat(0x08, 0x9b);
    cs.limit = 0xffff;
    cs.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ss = SegmentRegister::flat(0x10, 0x93);
    ss.limit = 0xffff;
    ss.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ss, ss);
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu
}

/// A 32-bit flat protected-mode CPU.
fn flat32_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
    }
    cpu
}

/// One CPU with its bus and the pristine memory image it restarts from.
struct Rig {
    cpu: CpuGsw,
    bus: TestBus,
    image: Vec<u8>,
}

impl Rig {
    /// `native` arms the fast map and admits blocks. Otherwise the CPU is a pure interpreter.
    /// Both kinds get the same bus configuration, so the clocks they report are comparable.
    fn new(mut cpu: CpuGsw, image: Vec<u8>, native: bool, batch_clocks: bool) -> Self {
        let mut bus = sixteen_bit_bus(image.clone());
        bus.uniform_native_fetches = true;
        bus.direct_page_clocks = true;
        let pages: Vec<u32> = (0..image.len() as u32).step_by(0x1000).collect();
        if batch_clocks {
            bus.report_batch_clocks = true;
            bus.batch_bus_scale = (16, 105);
        }
        arm_native_sixteen_bit(&mut cpu, &mut bus, &pages);
        cpu.set_jit_auto_admit(native);
        cpu.set_native_backend_enabled(native);
        Self { cpu, bus, image }
    }

    /// Put the guest back at `entry` with all registers, flags, memory, clocks, the trace and the
    /// batch counters of the bus cleared. The decode cache and any installed block stay.
    fn reset(&mut self, entry: u32, remainder: u64) {
        self.bus.memory.copy_from_slice(&self.image);
        self.bus.trace.clear();
        self.bus.pending_irq = None;
        self.bus.io_touched = false;
        self.cpu.halted = false;
        self.cpu.interrupt_shadow = false;
        self.cpu.registers.gpr.fill(0);
        self.cpu.registers.set_esp(0x9000);
        self.cpu.registers.eflags = 0x202;
        self.cpu.pending_flags = PendingFlags::default();
        self.cpu.set_eip(entry);
        self.cpu.elapsed_clocks = 0;
        self.cpu.core_clocks_so_far = 0;
        self.cpu.timing_rem = remainder;
        self.cpu.fp_rem = 0;
    }
}

/// Settle both CPUs and compare every quantity a guest or the timing model can see.
fn assert_rigs_agree(sut: &mut Rig, oracle: &mut Rig, context: &str) {
    sut.cpu.materialize_flags();
    oracle.cpu.materialize_flags();
    assert_eq!(
        crate::tests::settled_registers(&sut.cpu),
        crate::tests::settled_registers(&oracle.cpu),
        "{context}: registers"
    );
    assert_eq!(sut.cpu.eflags(), oracle.cpu.eflags(), "{context}: EFLAGS");
    assert_eq!(
        sut.cpu.elapsed_clocks, oracle.cpu.elapsed_clocks,
        "{context}: elapsed clocks"
    );
    assert_eq!(
        sut.cpu.timing_rem, oracle.cpu.timing_rem,
        "{context}: timing remainder"
    );
    assert_eq!(
        sut.cpu.perf.instructions, oracle.cpu.perf.instructions,
        "{context}: retired instructions"
    );
    assert_eq!(
        sut.bus.trace.elapsed_clocks(),
        oracle.bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    assert_eq!(sut.bus.memory, oracle.bus.memory, "{context}: guest RAM");
}

/// Compile the block at `entry`, check its shape, install it, and return the compilation facts.
fn install_block(
    cpu: &mut CpuGsw,
    entry: u32,
    d: bool,
    instructions: u8,
) -> (jit::direct::CompiledBlock, [u16; 4], u8) {
    let compilation = match jit::direct::compile(cpu, entry, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(reason) => {
            panic!("the fixture block was a structural reject: {reason:?}")
        }
        jit::direct::CompileOutcome::Retry(cause) => {
            panic!("the fixture walk gave up: {cause:?}")
        }
    };
    assert_eq!(
        compilation.span.instructions, instructions,
        "the block shape moved, so the assertions below are about a different block"
    );
    let sites = compilation.jcc_shadow_sites();
    let callouts = compilation.callout_interpret_one_slots;
    let key = jit::direct::key_for(cpu, entry, d).expect("a key for the fixture block");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("install the fixture block");
    let block = cpu.jit_direct.block(id).expect("the block must be live");
    (block, sites, callouts)
}

// ---------------------------------------------------------------------------
// H1. A budget stops a native loop where the stepping oracle stops.
// ---------------------------------------------------------------------------

mod cap_cycle {
    use super::*;

    /// 34 bytes in a loop: ALU on registers, a dword ALU op, PUSH, POP, PUSHA, POPA, RCR, a
    /// memory SUB, TEST, PUSHF, POP and a JMP back to the start. No port I/O, no STI, no POPF and
    /// no paging, so the only way the run can end is the budget rule.
    const MIXED: [u8; 34] = [
        0xb8, 0xfe, 0xff, 0xb4, 0x7f, 0xb3, 0xe1, 0x00, 0xd8, 0x30, 0xe0, 0x66, 0x05, 0x42, 0x12,
        0x00, 0x80, 0x50, 0x5a, 0x60, 0x61, 0xd1, 0xda, 0x29, 0x16, 0x00, 0x20, 0x85, 0xc0, 0x9c,
        0x59, 0x90, 0xeb, 0xde,
    ];
    const MIXED_STARTS: [u32; 17] = [
        0, 3, 5, 7, 9, 11, 17, 18, 19, 20, 21, 23, 27, 29, 30, 31, 32,
    ];
    const TINY_STARTS: [u32; 5] = [0, 3, 6, 8, 9];

    /// `mov ax,1; mov cx,3; add ax,cx; nop; jmp start`: one block that loops on itself.
    const TINY: [u8; 11] = [0xb8, 1, 0, 0xb9, 3, 0, 0x01, 0xc8, 0x90, 0xeb, 0xf5];

    fn pair(code: &[u8]) -> (Rig, Rig) {
        let mut image = vec![0u8; 0x10000];
        image[..code.len()].copy_from_slice(code);
        image[0x2000..0x2004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        (
            Rig::new(pm16_flat_cpu(), image.clone(), true, true),
            Rig::new(pm16_flat_cpu(), image, false, true),
        )
    }

    /// Run both CPUs through the same warm-up, so the decode caches match, and make sure Direct
    /// installed a block. The run loop stops at a decode-cache miss, so every instruction is
    /// decoded first. Then reset the guest. The warm-up uses real `run_budgeted` calls.
    fn prime(sut: &mut Rig, oracle: &mut Rig, starts: &[u32]) {
        for rig in [&mut *sut, &mut *oracle] {
            warm_sixteen_bit(&mut rig.cpu, &mut rig.bus, starts);
            for _ in 0..12 {
                rig.reset(0, 0);
                rig.cpu.run_budgeted(&mut rig.bus, 4096).unwrap();
            }
        }
        assert!(
            sut.cpu.jit_direct.len() > 0,
            "the warm-up installed no block: {:#?}",
            sut.cpu.perf_counters()
        );
        assert_eq!(oracle.cpu.jit_direct.len(), 0);
        sut.reset(0, 0);
        oracle.reset(0, 0);
    }

    /// Step `oracle` for `retired` instructions and check the budget rule at each one. The run
    /// must not have been allowed to stop before the last instruction, and it must have stopped
    /// at the last. Returns the core clocks the stepped instructions used.
    fn step_and_check_rule(oracle: &mut Rig, retired: u64, cap: u64, context: &str) -> u64 {
        let s0 = oracle.bus.in_batch_scaled_bus_clocks();
        let mut total = 0;
        for step in 1..=retired {
            total += oracle
                .cpu
                .cycle_no_interrupt_check(&mut oracle.bus)
                .unwrap()
                .core_clocks;
            let spent = total + oracle.bus.in_batch_scaled_bus_clocks() - s0;
            if step < retired {
                assert!(
                    spent < cap,
                    "{context}: the run kept going past the budget would have stopped it \
                     (instruction {step} of {retired}, spent {spent}, cap {cap}), so the native \
                     code did not stop at the boundary"
                );
            } else {
                assert!(
                    spent >= cap,
                    "{context}: the run stopped before the budget was used \
                     (spent {spent}, cap {cap}, retired {retired})"
                );
            }
        }
        total
    }

    #[test]
    fn caps_stop_a_mixed_native_loop_where_the_stepping_oracle_stops() {
        let (mut sut, mut oracle) = pair(&MIXED);
        prime(&mut sut, &mut oracle, &MIXED_STARTS);
        let entries = sut.cpu.perf_counters().jit_direct_entries;
        let native = sut.cpu.perf_counters().jit_direct_insns;
        for (index, cap) in [1u64, 3, 9, 13, 31, 79, 137, 509, 1999, 4001]
            .into_iter()
            .cycle()
            .take(170)
            .enumerate()
        {
            let context = format!("run {index}, cap {cap}");
            let before = sut.cpu.perf.instructions;
            let result = sut.cpu.run_budgeted(&mut sut.bus, cap).unwrap();
            let retired = sut.cpu.perf.instructions - before;
            assert!(retired > 0, "{context}: no instruction retired");
            let core = step_and_check_rule(&mut oracle, retired, cap, &context);
            assert_eq!(result.consumed_core_clocks, core, "{context}: core clocks");
            assert_rigs_agree(&mut sut, &mut oracle, &context);
        }
        let perf = sut.cpu.perf_counters();
        assert!(
            perf.jit_direct_entries - entries > 10,
            "Direct entered too few blocks: {perf:#?}"
        );
        assert!(
            perf.jit_direct_insns - native > 100,
            "Direct retired too few native instructions: {perf:#?}"
        );
    }

    #[test]
    fn caps_stop_a_self_loop_where_an_independent_oracle_stops() {
        let (mut sut, mut oracle) = pair(&TINY);
        prime(&mut sut, &mut oracle, &TINY_STARTS);
        let entries = sut.cpu.perf_counters().jit_direct_entries;
        let native = sut.cpu.perf_counters().jit_direct_insns;
        let mut rows = 0u64;
        for remainder in [0, 1, 7, 11] {
            // The native entry prices a whole iteration at a pessimistic upper bound, so the
            // budget only decides the quota once the cap is a few hundred clocks. The sweep
            // reaches that range too.
            let caps = (0..80u64)
                .chain((80..1200).step_by(37))
                .chain([1500, 2501, 4000]);
            for cap in caps {
                let context = format!("cap {cap}, remainder {remainder}");
                sut.reset(0, remainder);
                oracle.reset(0, remainder);
                let before = sut.cpu.perf.instructions;
                let result = sut.cpu.run_budgeted(&mut sut.bus, cap).unwrap();
                let retired = sut.cpu.perf.instructions - before;
                assert!(retired > 0, "{context}: no instruction retired");
                let core = step_and_check_rule(&mut oracle, retired, cap, &context);
                assert_eq!(result.consumed_core_clocks, core, "{context}: core clocks");
                assert_rigs_agree(&mut sut, &mut oracle, &context);
                rows += 1;
            }
        }
        let perf = sut.cpu.perf_counters();
        let entered = perf.jit_direct_entries - entries;
        let retired_native = perf.jit_direct_insns - native;
        // The block is five instructions. More than five per entry means a run went around the
        // loop more than once inside one entry, which is the case where a chain quota is
        // larger than one.
        assert!(
            entered > rows / 4,
            "Direct entered the block on too few rows: {entered} of {rows}: {perf:#?}"
        );
        assert!(
            retired_native > 5 * entered,
            "no entry ran more than one iteration ({retired_native} native instructions in \
             {entered} entries): {perf:#?}"
        );
    }
}

// ---------------------------------------------------------------------------
// H2. Memory-source ALU followed by Jcc, every condition and width.
// ---------------------------------------------------------------------------

mod condition_matrix {
    use super::jit_jcc_shadow::{expected_class, predicate};
    use super::*;

    const ENTRY: u32 = 0x100;
    const OPERAND: u32 = 0x6000;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Op {
        Add,
        Sub,
        Cmp,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Width {
        Byte,
        Word,
        Dword,
    }

    impl Width {
        fn mask(self) -> u32 {
            match self {
                Width::Byte => 0xff,
                Width::Word => 0xffff,
                Width::Dword => u32::MAX,
            }
        }
        fn sign(self) -> u32 {
            (self.mask() >> 1) + 1
        }
    }

    /// The independent flag oracle: the arithmetic written from the instruction definitions.
    fn flags_of(op: Op, width: Width, a: u32, b: u32) -> u32 {
        let mask = u64::from(width.mask());
        let sign = u64::from(width.sign());
        let (a, b) = (u64::from(a) & mask, u64::from(b) & mask);
        let (result, carry, overflow) = match op {
            Op::Add => {
                let sum = a + b;
                let r = sum & mask;
                (r, sum > mask, (!(a ^ b) & (a ^ r) & sign) != 0)
            }
            Op::Sub | Op::Cmp => {
                let r = a.wrapping_sub(b) & mask;
                (r, a < b, ((a ^ b) & (a ^ r) & sign) != 0)
            }
        };
        let mut flags = 0;
        if carry {
            flags |= crate::FLAG_CF;
        }
        if overflow {
            flags |= crate::FLAG_OF;
        }
        if result == 0 {
            flags |= crate::FLAG_ZF;
        }
        if result & sign != 0 {
            flags |= crate::FLAG_SF;
        }
        if (result as u8).count_ones().is_multiple_of(2) {
            flags |= crate::FLAG_PF;
        }
        flags
    }

    /// The signed-boundary operand set (destination, memory source).
    ///
    /// - `(sign, 1)`: SUB and CMP give SF=0 and OF=1. This is the pair that kills a wrong shift
    ///   count in the JL and JGE lowering, because only this point separates a correct SF^OF
    ///   from a misread bit.
    /// - `(0, 0)` and `(7, 3)`: ZF=1 then ZF=0 with SF=OF=0. `(0, 0)` kills a JG and JGE
    ///   (or JLE and JL) swap, because JLE is taken and JG is not. `(7, 3)` is the opposite.
    /// - `(sign - 1, mask)` and `(sign, mask)`: carry and overflow together for ADD and SUB.
    fn pairs(width: Width) -> [(u32, u32); 8] {
        let (mask, sign) = (width.mask(), width.sign());
        [
            (0, 0),
            (0, 1),
            (1, 0),
            (sign, 1),
            (sign - 1, mask),
            (7, 3),
            (sign - 1, 1),
            (sign, mask),
        ]
    }

    struct ShadowKnob;

    impl ShadowKnob {
        fn set(on: bool) -> Self {
            jit::direct::set_jcc_shadow_for_test(Some(on));
            ShadowKnob
        }
    }

    impl Drop for ShadowKnob {
        fn drop(&mut self) {
            jit::direct::set_jcc_shadow_for_test(None);
        }
    }

    /// `op AL|AX|EAX,[OPERAND]; Jcc +1; HLT; HLT`, placed at `entry`. The address form follows the
    /// code size. Returns the bytes and the address of the Jcc.
    fn program(d32: bool, entry: u32, op: Op, width: Width, condition: u8) -> (Vec<u8>, u32) {
        let mut code = Vec::new();
        let dword_in_16 = !d32 && width == Width::Dword;
        let word_in_32 = d32 && width == Width::Word;
        if dword_in_16 || word_in_32 {
            code.push(0x66);
        }
        let opcode = match op {
            Op::Add => 0x02,
            Op::Sub => 0x2a,
            Op::Cmp => 0x3a,
        } + u8::from(width != Width::Byte);
        code.push(opcode);
        if d32 {
            code.push(0x05);
            code.extend_from_slice(&OPERAND.to_le_bytes());
        } else {
            code.push(0x06);
            code.extend_from_slice(&(OPERAND as u16).to_le_bytes());
        }
        let jcc_at = entry + code.len() as u32;
        code.extend_from_slice(&[0x70 | condition, 1, 0xf4, 0xf4]);
        (code, jcc_at)
    }

    /// One block of the sweep.
    struct Spec {
        op: Op,
        width: Width,
        condition: u8,
        entry: u32,
        jcc_at: u32,
    }

    /// One (shadow arm, code size) sweep: 144 blocks, all in one pair of CPUs.
    fn sweep(on: bool, d32: bool) {
        let _knob = ShadowKnob::set(on);
        let mut image = vec![0u8; 0x8000];
        let mut specs = Vec::new();
        for op in [Op::Add, Op::Sub, Op::Cmp] {
            for width in [Width::Byte, Width::Word, Width::Dword] {
                for condition in 0..16u8 {
                    let entry = ENTRY + specs.len() as u32 * 0x20;
                    let (code, jcc_at) = program(d32, entry, op, width, condition);
                    image[entry as usize..entry as usize + code.len()].copy_from_slice(&code);
                    specs.push(Spec {
                        op,
                        width,
                        condition,
                        entry,
                        jcc_at,
                    });
                }
            }
        }
        let cpu = || if d32 { flat32_cpu() } else { pm16_flat_cpu() };
        let mut sut = Rig::new(cpu(), image.clone(), true, false);
        let mut oracle = Rig::new(cpu(), image.clone(), false, false);
        for rig in [&mut sut, &mut oracle] {
            rig.cpu.registers.set_esp(0x9000);
            for spec in &specs {
                let starts = [spec.entry, spec.jcc_at, spec.jcc_at + 2, spec.jcc_at + 3];
                warm_sixteen_bit(&mut rig.cpu, &mut rig.bus, &starts);
            }
            // Decoding marks the code page as watched, and a mark clears fast-map entries of that
            // page, so the pages are mapped again after the decode.
            let pages: Vec<u32> = (0..rig.image.len() as u32).step_by(0x1000).collect();
            arm_native_sixteen_bit(&mut rig.cpu, &mut rig.bus, &pages);
        }
        for spec in &specs {
            run_block(&mut sut, &mut oracle, on, d32, spec);
        }
    }

    fn run_block(sut: &mut Rig, oracle: &mut Rig, on: bool, d32: bool, spec: &Spec) {
        let Spec {
            op,
            width,
            condition,
            entry,
            jcc_at,
        } = *spec;
        let label = format!("shadow={on} d32={d32} {op:?} {width:?} cc={condition:#x}");
        let fallthrough = jcc_at + 2;
        let taken = fallthrough + 1;
        let (block, sites, callouts) = install_block(&mut sut.cpu, entry, d32, 2);
        // The memory-source ALU slot must be emitted natively, not left to an interpreter
        // call-out. With a call-out the block would still compare equal.
        assert_eq!(callouts, 0, "{label}: the ALU slot became a call-out");
        // The shadow lowering must have engaged on the ON arm, and must be absent on the OFF arm.
        let mut want_sites = [0u16; 4];
        if on {
            want_sites[expected_class(condition)] = 1;
        }
        assert_eq!(sites, want_sites, "{label}: the Jcc lowering arm");

        let (mut taken_count, mut fell_count) = (0, 0);
        for (a, b) in pairs(width) {
            let context = format!("{label} a={a:#x} b={b:#x}");
            for rig in [&mut *sut, &mut *oracle] {
                rig.reset(entry, 0);
                rig.cpu.registers.set_eax(a);
                let at = OPERAND as usize;
                rig.bus.memory[at..at + 4].copy_from_slice(&b.to_le_bytes());
                // The reset leaves the memory image alone for the pristine bytes, so the operand
                // is written here and the pending flags start clean.
            }
            let flags = flags_of(op, width, a, b);
            let expect_taken = predicate(condition, flags);
            let insns = sut.cpu.perf.instructions;
            let native = sut.cpu.perf_counters().jit_direct_insns;
            assert!(
                sut.cpu
                    .try_run_direct_block_for_test(&mut sut.bus, block)
                    .unwrap(),
                "{context}: the block did not run"
            );
            assert_eq!(
                sut.cpu.perf_counters().jit_direct_insns - native,
                2,
                "{context}: the block did not retire both instructions natively (exit {:?})",
                sut.cpu.jit_direct.last_side_exit_reason_for_test()
            );
            assert_eq!(sut.cpu.perf.instructions - insns, 2, "{context}: retired");
            for _ in 0..2 {
                oracle
                    .cpu
                    .cycle_no_interrupt_check(&mut oracle.bus)
                    .unwrap();
            }
            let want_eip = if expect_taken { taken } else { fallthrough };
            assert_eq!(sut.cpu.registers.eip, want_eip, "{context}: native EIP");
            assert_eq!(oracle.cpu.registers.eip, want_eip, "{context}: oracle EIP");
            if expect_taken {
                taken_count += 1;
            } else {
                fell_count += 1;
            }
            // The flags the guest sees must be the independent ones, before any settling.
            let seen = sut.cpu.eflags()
                & (crate::FLAG_CF
                    | crate::FLAG_OF
                    | crate::FLAG_ZF
                    | crate::FLAG_SF
                    | crate::FLAG_PF);
            assert_eq!(seen, flags, "{context}: flags");
            assert_rigs_agree(sut, oracle, &context);
        }
        assert!(
            taken_count > 0 && fell_count > 0,
            "{label}: the operand set never separates taken from not taken \
             ({taken_count} taken, {fell_count} not taken)"
        );
    }

    #[test]
    fn memory_alu_then_jcc_matches_in_16_bit_protected_mode_on_both_arms() {
        for on in [false, true] {
            sweep(on, false);
        }
    }

    #[test]
    fn memory_alu_then_jcc_matches_in_32_bit_protected_mode_on_both_arms() {
        for on in [false, true] {
            sweep(on, true);
        }
    }
}

// ---------------------------------------------------------------------------
// H3. A guest fact changes after a block is installed.
// ---------------------------------------------------------------------------

mod live_guards {
    use super::jit_direct::interpret_one as io;
    use super::jit_direct::interpret_one::live_pm_segments as live;
    use super::*;

    const ENTRY: u32 = 0x100;
    const IMAGE_LEN: usize = 0x3000;
    const HANDLER_BASE: u32 = 0x800;
    const VECTORS: [u8; 4] = [11, 12, 13, 17];

    /// One distinct HLT stub per vector, so the final EIP names the vector that was delivered.
    fn handler(vector: u8) -> u32 {
        HANDLER_BASE + u32::from(vector) * 0x10
    }

    /// The guest image: the tables of the live segment tests, room for a third data page, a gate
    /// and a HLT stub for each vector the cases can raise, and `code` at `ENTRY`. `gdt_slot`
    /// replaces one descriptor.
    fn guest_image(selector_word: u16, code: &[u8], gdt_slot: Option<(u16, [u8; 8])>) -> Vec<u8> {
        let mut memory = live::image(selector_word);
        memory.resize(IMAGE_LEN, 0);
        for vector in VECTORS {
            live::put(
                &mut memory,
                io::IDT_BASE + u32::from(vector) * 8,
                io::interrupt_gate(handler(vector)),
            );
            memory[handler(vector) as usize] = 0xf4;
        }
        if let Some((selector, descriptor)) = gdt_slot {
            live::put(&mut memory, io::GDT_BASE + u32::from(selector), descriptor);
        }
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
        memory
    }

    /// What a delivered exception leaves behind: the handler position and the 32-bit frame.
    #[derive(Debug, PartialEq, Eq)]
    struct Frame {
        eip: u32,
        esp: u32,
        cs: u16,
        /// Error code, EIP, CS and EFLAGS as the gate pushed them.
        stacked: [u32; 4],
    }

    fn frame(rig: &Rig) -> Frame {
        let esp = rig.cpu.registers.esp();
        let mut stacked = [0u32; 4];
        for (index, word) in stacked.iter_mut().enumerate() {
            let at = esp as usize + index * 4;
            *word = u32::from_le_bytes(rig.bus.memory[at..at + 4].try_into().unwrap());
        }
        Frame {
            eip: rig.cpu.registers.eip,
            esp,
            cs: rig.cpu.registers.cs().selector,
            stacked,
        }
    }

    /// The facts about one run that a row pins. Deltas, taken after warm-up.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    struct Delta {
        entries: u64,
        native_insns: u64,
        side_exits: u64,
        reject_data_segment: u64,
        reject_alignment: u64,
    }

    fn counters(cpu: &CpuGsw) -> Delta {
        let perf = cpu.perf_counters();
        Delta {
            entries: perf.jit_direct_entries,
            native_insns: perf.jit_direct_insns,
            side_exits: perf.jit_direct_side_exits,
            reject_data_segment: perf.jit_direct_reject_data_segment,
            reject_alignment: perf.jit_direct_reject_alignment,
        }
    }

    fn delta(after: Delta, before: Delta) -> Delta {
        Delta {
            entries: after.entries - before.entries,
            native_insns: after.native_insns - before.native_insns,
            side_exits: after.side_exits - before.side_exits,
            reject_data_segment: after.reject_data_segment - before.reject_data_segment,
            reject_alignment: after.reject_alignment - before.reject_alignment,
        }
    }

    const fn delta_of(
        entries: u64,
        native_insns: u64,
        side_exits: u64,
        reject_data_segment: u64,
        reject_alignment: u64,
    ) -> Delta {
        Delta {
            entries,
            native_insns,
            side_exits,
            reject_data_segment,
            reject_alignment,
        }
    }

    /// Run `rig` to its HLT, or, when `steps_only` is set, until `wanted` instructions have
    /// retired since `start`. The CPL 3 rows cannot end on a HLT. Admission is off, so the
    /// installed block cannot be entered again behind the counters.
    fn finish(rig: &mut Rig, start: u64, steps_only: bool, wanted: u64) {
        rig.cpu.set_jit_auto_admit(false);
        if steps_only {
            while rig.cpu.perf.instructions - start < wanted {
                rig.cpu.run_straight_line(&mut rig.bus, 1).unwrap();
            }
            return;
        }
        for _ in 0..64 {
            if rig
                .cpu
                .run_straight_line(&mut rig.bus, u64::MAX)
                .unwrap()
                .halted
            {
                return;
            }
        }
        panic!(
            "the guest did not halt: eip {:#x}, halted {}",
            rig.cpu.registers.eip, rig.cpu.halted
        );
    }

    /// The guest saw the same exit on both CPUs, and it is the one the row says. Every baked row
    /// that faults does so on the CMP at `ENTRY`.
    fn check_exit(name: &str, sut: &Rig, oracle: &Rig, vector: Option<u8>, steps_only: bool) {
        if steps_only {
            assert_eq!(sut.cpu.registers.eip, oracle.cpu.registers.eip, "{name}");
            return;
        }
        let (native, stepped) = (frame(sut), frame(oracle));
        assert_eq!(native, stepped, "{name}: the exit frame");
        match vector {
            Some(vector) => {
                assert_eq!(native.eip, handler(vector) + 1, "{name}: the vector");
                assert_eq!(native.cs, io::SEL_CODE, "{name}: the handler CS");
                assert_eq!(native.stacked[1], ENTRY, "{name}: stacked EIP");
                assert_eq!(
                    native.stacked[2],
                    u32::from(io::SEL_CODE),
                    "{name}: the stacked CS"
                );
            }
            None => assert!(
                VECTORS
                    .iter()
                    .all(|&vector| native.eip != handler(vector) + 1),
                "{name}: an exception was delivered"
            ),
        }
    }

    // -----------------------------------------------------------------------
    // The baked arm: the block is compiled and run once, then one fact changes.
    // -----------------------------------------------------------------------

    /// `CMP AX,[BX]; JA +1; HLT; HLT`.
    const CMP_JA: [u8; 6] = [0x3b, 0x07, 0x77, 0x01, 0xf4, 0xf4];
    const CMP_JA_STARTS: [u32; 4] = [ENTRY, ENTRY + 2, ENTRY + 4, ENTRY + 5];
    /// A word in the data pattern.
    const OPERAND: u32 = 0x1a20;

    struct Fact {
        name: &'static str,
        /// The CPU runs at CPL 3, and the guest cannot end on a HLT.
        cpl3: bool,
        ebx: u32,
        /// The vector the guest must see, if any.
        vector: Option<u8>,
        apply: fn(&mut CpuGsw, &mut TestBus),
        /// The counter deltas of the run after the change.
        expect: Delta,
    }

    fn set_ds(cpu: &mut CpuGsw, change: impl FnOnce(&mut SegmentRegister)) {
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        change(&mut ds);
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
    }

    fn repopulate_read(cpu: &mut CpuGsw, bus: &mut TestBus, user: bool, epoch_step: u64) {
        let mut page = bus
            .direct_page(0x1000, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        page.mapping_epoch += epoch_step;
        cpu.jit_fast_map.invalidate_page(0x1000);
        assert!(cpu.jit_fast_map.populate_read(
            0x1000,
            0x1000,
            page,
            jit::fast_map::PagePermissions {
                writable: true,
                user
            },
            false
        ));
    }

    fn facts() -> Vec<Fact> {
        vec![
            Fact {
                name: "fast map page invalid",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, _| cpu.jit_fast_map.invalidate_page(0x1000),
                expect: delta_of(1, 0, 1, 0, 0),
            },
            Fact {
                name: "misaligned address",
                cpl3: false,
                ebx: OPERAND + 1,
                vector: None,
                apply: |_, _| {},
                expect: delta_of(1, 2, 0, 0, 0),
            },
            Fact {
                name: "word crosses the page end",
                cpl3: false,
                ebx: 0x1fff,
                vector: None,
                apply: |_, _| {},
                expect: delta_of(1, 0, 1, 0, 0),
            },
            Fact {
                name: "new DS base",
                cpl3: false,
                ebx: OPERAND - 0x100,
                vector: None,
                apply: |cpu, _| set_ds(cpu, |ds| ds.base = 0x100),
                expect: delta_of(0, 0, 0, 1, 0),
            },
            Fact {
                name: "smaller DS limit",
                cpl3: false,
                ebx: OPERAND,
                vector: Some(13),
                apply: |cpu, _| set_ds(cpu, |ds| ds.limit = 0x19ff),
                expect: delta_of(0, 0, 0, 1, 0),
            },
            // The only row that separates an entry check which compares the access byte from
            // one which does not: the base, the limit and the size bit stay the same, and the
            // segment stops being readable.
            Fact {
                name: "execute-only DS",
                cpl3: false,
                ebx: OPERAND,
                vector: Some(13),
                apply: |cpu, _| set_ds(cpu, |ds| ds.access = 0x98),
                expect: delta_of(0, 0, 0, 1, 0),
            },
            // The offset is below the limit, so it is invalid for an expand-down segment. A
            // check that reads the limit as a plain upper bound would run the access.
            Fact {
                name: "expand-down DS, offset inside the limit",
                cpl3: false,
                ebx: OPERAND,
                vector: Some(13),
                apply: |cpu, _| {
                    set_ds(cpu, |ds| {
                        ds.access = 0x96;
                        ds.limit = 0x1fff;
                    })
                },
                expect: delta_of(0, 0, 0, 1, 0),
            },
            Fact {
                name: "expand-down DS, offset above the limit",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, _| {
                    set_ds(cpu, |ds| {
                        ds.access = 0x96;
                        ds.limit = 0x19ff;
                    })
                },
                expect: delta_of(0, 0, 0, 1, 0),
            },
            Fact {
                name: "read census on",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, _| cpu.rmw_census_enabled = true,
                expect: delta_of(1, 2, 0, 0, 0),
            },
            Fact {
                name: "slot census on",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, _| cpu.slot_census_enabled = true,
                expect: delta_of(1, 2, 0, 0, 0),
            },
            // The access is aligned, so no #AC is due. Direct refuses a block with a wide access
            // while alignment checking is armed at CPL 3, and the interpreter runs it.
            Fact {
                name: "alignment check armed at CPL 3",
                cpl3: true,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, _| {
                    cpu.control.cr0 |= CR0_AM;
                    cpu.registers.eflags |= FLAG_AC;
                    cpu.recompute_alignment_armed();
                },
                expect: delta_of(0, 0, 0, 0, 1),
            },
            Fact {
                name: "page mapping epoch moves",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, bus| repopulate_read(cpu, bus, true, 1),
                expect: delta_of(1, 2, 0, 0, 0),
            },
            Fact {
                name: "bus mapping epoch moves",
                cpl3: false,
                ebx: OPERAND,
                vector: None,
                apply: |_, bus| bus.direct_mapping_epoch += 1,
                expect: delta_of(1, 2, 0, 0, 0),
            },
            // Paging is off here. This row tests the fast-map permission guard on its own: a
            // supervisor entry must not serve a CPL 3 access natively. It is not a paging test.
            Fact {
                name: "supervisor fast-map entry at CPL 3",
                cpl3: true,
                ebx: OPERAND,
                vector: None,
                apply: |cpu, bus| repopulate_read(cpu, bus, false, 0),
                expect: delta_of(1, 0, 1, 0, 0),
            },
        ]
    }

    fn arm_registers(rig: &mut Rig, eax: u32, ebx: u32) {
        rig.reset(ENTRY, 7);
        rig.cpu.registers.set_esp(io::STACK_TOP);
        rig.cpu.registers.set_eax(eax);
        rig.cpu.registers.set_ebx(ebx);
    }

    fn warm_and_arm(rig: &mut Rig, starts: &[u32]) {
        warm_sixteen_bit(&mut rig.cpu, &mut rig.bus, starts);
        // Decoding marks the code page as watched, and a mark clears fast-map entries of that
        // page, so the pages are mapped after the decode.
        let pages: Vec<u32> = (0..rig.image.len() as u32).step_by(0x1000).collect();
        arm_native_sixteen_bit(&mut rig.cpu, &mut rig.bus, &pages);
    }

    fn run_fact(fact: &Fact) {
        let _knob = live::Knob::set(false);
        let image = guest_image(io::SEL_DATA, &CMP_JA, None);
        let cpu = || live::pm16_cpu(fact.cpl3);
        let mut sut = Rig::new(cpu(), image.clone(), true, false);
        let mut oracle = Rig::new(cpu(), image, false, false);
        for rig in [&mut sut, &mut oracle] {
            // The production bus charges a misaligned or page-crossing access the same on both
            // paths. The default dial of the test bus does not, so the rows use the flat one.
            rig.bus.flat_direct_page_clocks = true;
            warm_and_arm(rig, &CMP_JA_STARTS);
        }
        let (block, _, callouts) = install_block(&mut sut.cpu, ENTRY, false, 2);
        assert_eq!(callouts, 0, "{}: call-outs in the block", fact.name);

        // Run once with nothing changed. The block must run natively here, or the rerun below
        // measures a block that never worked.
        for rig in [&mut sut, &mut oracle] {
            arm_registers(rig, 0x5555, OPERAND);
        }
        let start = counters(&sut.cpu);
        assert!(
            sut.cpu
                .try_run_direct_block_for_test(&mut sut.bus, block)
                .unwrap(),
            "{}: the control run did not enter the block",
            fact.name
        );
        let control = delta(counters(&sut.cpu), start);
        assert_eq!(
            (control.entries, control.native_insns),
            (1, 2),
            "{}: the control run was not native",
            fact.name
        );
        for _ in 0..2 {
            oracle
                .cpu
                .cycle_no_interrupt_check(&mut oracle.bus)
                .unwrap();
        }

        // Change one fact on both CPUs and run again.
        for rig in [&mut sut, &mut oracle] {
            arm_registers(rig, 0x5555, fact.ebx);
            (fact.apply)(&mut rig.cpu, &mut rig.bus);
        }
        let retired = sut.cpu.perf.instructions;
        let before = counters(&sut.cpu);
        sut.cpu
            .try_run_direct_block_for_test(&mut sut.bus, block)
            .unwrap();
        let oracle_start = oracle.cpu.perf.instructions;
        finish(&mut sut, retired, fact.cpl3, 2);
        finish(&mut oracle, oracle_start, fact.cpl3, 2);
        let seen = delta(counters(&sut.cpu), before);
        assert_eq!(
            seen, fact.expect,
            "{}: counter deltas of the run after the change",
            fact.name
        );
        assert_rigs_agree(&mut sut, &mut oracle, fact.name);
        check_exit(fact.name, &sut, &oracle, fact.vector, fact.cpl3);
    }

    #[test]
    fn a_changed_fact_after_install_matches_the_interpreter() {
        for fact in facts() {
            run_fact(&fact);
        }
    }

    /// With admission off the run loop must not enter an installed block. With it on, the same
    /// loop must enter it, or the first half of the test proves nothing.
    #[test]
    fn admission_off_keeps_the_run_loop_out_of_an_installed_block() {
        let _knob = live::Knob::set(false);
        let image = guest_image(io::SEL_DATA, &CMP_JA, None);
        let mut sut = Rig::new(live::pm16_cpu(false), image.clone(), true, false);
        let mut oracle = Rig::new(live::pm16_cpu(false), image, false, false);
        for rig in [&mut sut, &mut oracle] {
            warm_and_arm(rig, &CMP_JA_STARTS);
        }
        install_block(&mut sut.cpu, ENTRY, false, 2);
        for admit in [true, false] {
            sut.cpu.set_jit_auto_admit(admit);
            let before = counters(&sut.cpu);
            // The run loop enters a block on a later pass, so run the guest several times.
            for _ in 0..8 {
                for rig in [&mut sut, &mut oracle] {
                    arm_registers(rig, 0x5555, OPERAND);
                    for _ in 0..64 {
                        if rig.cpu.run_budgeted(&mut rig.bus, 4096).unwrap().halted {
                            break;
                        }
                    }
                    assert!(rig.cpu.halted, "the guest did not halt");
                }
                assert_rigs_agree(&mut sut, &mut oracle, "admission");
            }
            let seen = delta(counters(&sut.cpu), before);
            if admit {
                assert!(seen.entries > 0, "the run loop never entered the block");
                assert!(seen.native_insns > 0);
            } else {
                assert_eq!(seen, delta_of(0, 0, 0, 0, 0), "admission is off");
            }
        }
    }

    // -----------------------------------------------------------------------
    // A block that loads DS from a real GDT, on both arms of the live-segment knob.
    // -----------------------------------------------------------------------

    /// `NOP; MOV DS,AX; MOV AL,[DI]; MOV CX,[DI]; MOV EBP,[DI]; MOV [SI],CX; NOP; HLT`. The NOP
    /// in front means a fault in the DS load still leaves one native instruction behind it.
    const LOAD_AND_PROBE: [u8; 14] = [
        0x90, 0x8e, 0xd8, 0x8a, 0x05, 0x8b, 0x0d, 0x66, 0x8b, 0x2d, 0x89, 0x0c, 0x90, 0xf4,
    ];
    const LOAD_STARTS: [u32; 8] = [
        ENTRY,
        ENTRY + 1,
        ENTRY + 3,
        ENTRY + 5,
        ENTRY + 7,
        ENTRY + 10,
        ENTRY + 12,
        ENTRY + 13,
    ];

    struct Row {
        name: &'static str,
        selector: u16,
        di: u32,
        si: u32,
        /// Replace one GDT descriptor before the guest starts.
        gdt_slot: Option<(u16, [u8; 8])>,
        /// The vector the guest must see, and the offset of the instruction that faults.
        fault: Option<(u8, u32)>,
    }

    fn rows() -> Vec<Row> {
        vec![
            Row {
                name: "live new base",
                selector: live::SEL_B,
                di: 0x20,
                si: 0x80,
                gdt_slot: None,
                fault: None,
            },
            // The loads at 0x20 fit in the limit. The word store at 0x80 does not.
            Row {
                name: "live smaller limit",
                selector: live::SEL_SHORT,
                di: 0x20,
                si: 0x80,
                gdt_slot: None,
                fault: Some((13, ENTRY + 10)),
            },
            // Execute-only code cannot be loaded into DS. The load is the call-out and faults.
            Row {
                name: "live execute-only",
                selector: live::SEL_A,
                di: 0x20,
                si: 0x80,
                gdt_slot: Some((live::SEL_A, live::desc(0x1900, 0xff, 0x98, 0))),
                fault: Some((13, ENTRY + 1)),
            },
            // Offsets up to the limit are invalid for expand-down. The first load faults.
            Row {
                name: "live expand-down, offset inside the limit",
                selector: live::SEL_EXPAND_DOWN,
                di: 0x20,
                si: 0x80,
                gdt_slot: None,
                fault: Some((13, ENTRY + 3)),
            },
            // Offsets above the limit are valid. Nothing faults.
            Row {
                name: "live expand-down, offset above the limit",
                selector: live::SEL_EXPAND_DOWN,
                di: 0x120,
                si: 0x180,
                gdt_slot: None,
                fault: None,
            },
            Row {
                name: "live readable code, store",
                selector: live::SEL_CODE_READ,
                di: 0x20,
                si: 0x80,
                gdt_slot: None,
                fault: Some((13, ENTRY + 10)),
            },
        ]
    }

    fn run_row(row: &Row, on: bool) {
        let label = format!("{} (knob {})", row.name, if on { "on" } else { "off" });
        let _knob = live::Knob::set(on);
        let image = guest_image(io::SEL_DATA, &LOAD_AND_PROBE, row.gdt_slot);
        let mut sut = Rig::new(live::pm16_cpu(false), image.clone(), true, false);
        let mut oracle = Rig::new(live::pm16_cpu(false), image, false, false);
        for rig in [&mut sut, &mut oracle] {
            warm_and_arm(rig, &LOAD_STARTS);
        }
        let (block, _, callouts) = install_block(&mut sut.cpu, ENTRY, false, 7);
        assert_eq!(callouts, 1, "{label}: the DS load must be a call-out slot");
        for rig in [&mut sut, &mut oracle] {
            arm_registers(rig, u32::from(row.selector), 0);
            rig.cpu.registers.set_edi(row.di);
            rig.cpu.registers.set_esi(row.si);
        }
        let before = counters(&sut.cpu);
        assert!(
            sut.cpu
                .try_run_direct_block_for_test(&mut sut.bus, block)
                .unwrap(),
            "{label}: the block did not run"
        );
        let entered = delta(counters(&sut.cpu), before);
        assert_eq!(entered.entries, 1, "{label}: entries");
        assert!(entered.native_insns >= 1, "{label}: no native instruction");
        let start = sut.cpu.perf.instructions;
        finish(&mut sut, start, false, 0);
        let oracle_start = oracle.cpu.perf.instructions;
        finish(&mut oracle, oracle_start, false, 0);
        assert_rigs_agree(&mut sut, &mut oracle, &label);
        let native = frame(&sut);
        assert_eq!(native, frame(&oracle), "{label}: the exit frame");
        match row.fault {
            Some((vector, at)) => {
                assert_eq!(native.eip, handler(vector) + 1, "{label}: the vector");
                assert_eq!(native.stacked[1], at, "{label}: the stacked EIP");
                assert_eq!(
                    native.stacked[2],
                    u32::from(io::SEL_CODE),
                    "{label}: the stacked CS"
                );
            }
            None => assert_eq!(
                native.eip,
                ENTRY + 14,
                "{label}: the guest must reach its HLT"
            ),
        }
    }

    #[test]
    fn a_ds_load_from_the_gdt_matches_the_interpreter_with_the_live_knob_off() {
        for row in rows() {
            run_row(&row, false);
        }
    }

    #[test]
    fn a_ds_load_from_the_gdt_matches_the_interpreter_with_the_live_knob_on() {
        for row in rows() {
            run_row(&row, true);
        }
    }
}
