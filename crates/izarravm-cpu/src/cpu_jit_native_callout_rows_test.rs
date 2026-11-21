// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `IZARRAVM_NATIVE_CALLOUT_ROWS`: three `InterpretOne` rows that compile natively when their item
//! is on. Every case runs one program natively and interpreted and compares architectural state:
//! registers, flags, guest RAM (which holds any fault frame) and the retirement count. Clocks are
//! compared between the native leg and the interpreted leg of one run, never between a knob-on and
//! a knob-off run.
//!
//! This module is nested in `cpu_jit_interpret_one_test.rs` for its fixtures and constants.

use super::*;
use crate::{FLAG_IF, FLAG_VM};

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
mod rows {
    use super::*;

    const ALL: u8 = jit::direct::NATIVE_ROW_MOV_RM_SREG
        | jit::direct::NATIVE_ROW_CLI
        | jit::direct::NATIVE_ROW_XCHG_REG;
    const V86_BASE: u32 = 0xA000;
    const V86_TARGET: u32 = 0x0300;
    /// Extra GDT entries: code segments at DPL 1, 2, 3 and the TSS.
    const GDT_CODE1: u32 = GDT_BASE + 0x40;
    const GDT_TSS: u32 = GDT_BASE + 0x58;
    const TSS_BASE: u32 = 0x1600;
    const TSS_SEL: u16 = 0x58;
    const ESP0: u32 = 0x1f00;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Mode {
        Real,
        Pm16,
        Pm32,
        V86,
    }

    impl Mode {
        fn is_32(self) -> bool {
            self == Mode::Pm32
        }
    }

    #[derive(Clone, Copy)]
    struct Setup {
        mode: Mode,
        cpl: u8,
        iopl: u8,
        /// The privilege the block is compiled under. The run uses `cpl` and `iopl`.
        compile_cpl: u8,
        compile_iopl: u8,
        tweak: fn(&mut CpuGsw, &mut TestBus),
        /// Whether the run loads the CS record of `cpl`. A block compiled at CPL 0 keeps its CS
        /// record when the run only changes the cached CPL.
        cs_follows_cpl: bool,
    }

    fn setup(mode: Mode) -> Setup {
        Setup {
            mode,
            cpl: if mode == Mode::V86 { 3 } else { 0 },
            iopl: 0,
            compile_cpl: if mode == Mode::V86 { 3 } else { 0 },
            compile_iopl: 0,
            tweak: no_perturb,
            cs_follows_cpl: true,
        }
    }

    fn at(mode: Mode, cpl: u8, iopl: u8) -> Setup {
        Setup {
            cpl,
            iopl,
            compile_cpl: cpl,
            compile_iopl: iopl,
            ..setup(mode)
        }
    }

    struct World {
        cpu: CpuGsw,
        bus: TestBus,
        d: bool,
        lin: u32,
        eip: u32,
    }

    fn code_selector(cpl: u8) -> (u16, u8) {
        match cpl {
            0 => (SEL_CODE, 0x9b),
            1 => (0x41, 0xbb),
            2 => (0x4a, 0xdb),
            _ => (0x53, 0xfb),
        }
    }

    /// Set the privilege state that CLI reads: the cached CPL, CS, and IOPL in EFLAGS.
    fn set_privilege(cpu: &mut CpuGsw, mode: Mode, cpl: u8, iopl: u8, update_cs: bool) {
        if mode == Mode::Pm16 || mode == Mode::Pm32 {
            cpu.cpl = cpl;
            if !update_cs {
                cpu.registers.eflags = (cpu.registers.eflags & !0x3000) | (u32::from(iopl) << 12);
                return;
            }
            let (selector, access) = code_selector(cpl);
            let mut cs = SegmentRegister::flat(selector, access);
            cs.default_size_32 = mode.is_32();
            cpu.registers.set_segment(SegmentIndex::Cs, cs);
            cpu.cpl = cpl;
        }
        cpu.registers.eflags = (cpu.registers.eflags & !0x3000) | (u32::from(iopl) << 12);
    }

    fn pm_spread(cpu: &mut CpuGsw, _: &mut TestBus) {
        for (segment, selector) in [
            (SegmentIndex::Es, SEL_OTHER),
            (SegmentIndex::Fs, SEL_UNACCESSED),
            (SegmentIndex::Gs, SEL_SS16),
        ] {
            cpu.registers
                .set_segment(segment, SegmentRegister::flat(selector, 0x93));
        }
    }

    fn v86_spread(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.load_segment_real(SegmentIndex::Fs, 0x5678);
        cpu.load_segment_real(SegmentIndex::Gs, 0x9abc);
    }

    fn build(code: &[u8], s: Setup) -> World {
        match s.mode {
            Mode::Real => {
                let mut program = vec![0u8; 0x2000];
                program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
                seed_fault_handler(&mut program);
                let bus = sixteen_bit_bus(program);
                let mut cpu = sixteen_bit_code_cpu(ENTRY);
                cpu.load_segment_real(SegmentIndex::Es, 0x1234);
                cpu.load_segment_real(SegmentIndex::Fs, 0x5678);
                cpu.load_segment_real(SegmentIndex::Gs, 0x9abc);
                World {
                    cpu,
                    bus,
                    d: false,
                    lin: ENTRY,
                    eip: ENTRY,
                }
            }
            Mode::Pm16 | Mode::Pm32 => {
                let mut program = vec![0u8; 0x2000];
                program[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
                seed_protected_tables(&mut program);
                for (index, cpl) in [1u8, 2, 3].into_iter().enumerate() {
                    let (_, access) = code_selector(cpl);
                    let at = (GDT_CODE1 + index as u32 * 8) as usize;
                    program[at..at + 8].copy_from_slice(&descriptor(
                        0x0000_ffff,
                        0x00cf_0000 | (u32::from(access) << 8),
                    ));
                }
                let at = GDT_TSS as usize;
                program[at..at + 8]
                    .copy_from_slice(&descriptor((TSS_BASE << 16) | 0x67, 0x0000_8900));
                let tss = TSS_BASE as usize;
                program[tss + 4..tss + 8].copy_from_slice(&ESP0.to_le_bytes());
                program[tss + 8..tss + 10].copy_from_slice(&SEL_DATA.to_le_bytes());
                program[tss + 0x66..tss + 0x68].copy_from_slice(&0x68u16.to_le_bytes());
                let bus = sixteen_bit_bus(program);
                let mut cpu = protected_cpu();
                cpu.gdtr.limit = 0x5f;
                cpu.tr = SegmentRegister {
                    selector: TSS_SEL,
                    base: TSS_BASE,
                    limit: 0x67,
                    access: 0x89,
                    default_size_32: false,
                };
                set_privilege(&mut cpu, s.mode, s.compile_cpl, s.compile_iopl, true);
                World {
                    cpu,
                    bus,
                    d: s.mode.is_32(),
                    lin: ENTRY,
                    eip: ENTRY,
                }
            }
            Mode::V86 => {
                const MONITOR: &[u8] = &[0xF4];
                let (mut cpu, mut bus) = crate::tests::v86_world(MONITOR, code, &[0x00]);
                bus.direct_pages_enabled = true;
                crate::tests::enter_v86_direct(&mut cpu, 0, 0x1000);
                cpu.registers.eflags |= (u32::from(s.compile_iopl)) << 12;
                World {
                    cpu,
                    bus,
                    d: false,
                    lin: V86_BASE,
                    eip: 0,
                }
            }
        }
    }

    /// Put the registers, flags, clocks and fixture state in place for the run.
    fn arm(w: &mut World, s: Setup) {
        let cpu = &mut w.cpu;
        let bus = &mut w.bus;
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(w.eip);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
        bus.trace = BusTrace::default();
        let target = match s.mode {
            Mode::V86 => {
                cpu.registers.eflags = 0x202 | FLAG_VM;
                cpu.registers.set_esp(0x1000);
                V86_TARGET
            }
            Mode::Real => {
                cpu.registers.eflags = 0x202;
                cpu.registers.set_esp(STACK_TOP);
                POP_TARGET
            }
            _ => {
                cpu.registers.eflags = 0x202;
                cpu.registers.set_esp(STACK_TOP);
                POP_TARGET
            }
        };
        cpu.registers.set_ebx(target);
        let at = match s.mode {
            Mode::V86 => (V86_BASE + target) as usize,
            _ => target as usize,
        };
        bus.memory[at..at + 8].fill(0xee);
        set_privilege(cpu, s.mode, s.cpl, s.iopl, s.cs_follows_cpl);
        match s.mode {
            Mode::V86 => {
                cpu.cpl = 3;
                v86_spread(cpu, bus);
            }
            Mode::Pm16 | Mode::Pm32 => pm_spread(cpu, bus),
            Mode::Real => {}
        }
        (s.tweak)(cpu, bus);
    }

    struct Pair {
        interp: CpuGsw,
        interp_bus: TestBus,
        native: CpuGsw,
        native_bus: TestBus,
        native_insns: u64,
        exit_reason: Option<u32>,
        /// Instructions in the compiled block.
        instructions: u8,
        interpret_one_slots: u8,
        used: u8,
        chain_used: u8,
        target: usize,
    }

    struct RowsGuard;
    impl Drop for RowsGuard {
        fn drop(&mut self) {
            jit::direct::set_native_callout_rows_for_test(None);
        }
    }

    fn compile_native(code: &[u8], starts: &[u32], s: Setup, rows: Option<u8>) -> NativeBuild {
        let _guard = RowsGuard;
        jit::direct::set_native_callout_rows_for_test(rows);
        let mut w = build(code, s);
        let pages: &[u32] = &[0, DATA_PAGE];
        arm_native_sixteen_bit(&mut w.cpu, &mut w.bus, pages);
        let saved = w.cpu.registers.eip;
        for &offset in starts {
            w.cpu.set_eip(w.eip + offset);
            w.cpu.begin_instruction();
            w.cpu
                .fetch_decoded(&mut w.bus, w.lin + offset)
                .expect("fixture decode");
        }
        w.cpu.set_eip(saved);
        let compilation = jit::direct::compile(&mut w.cpu, w.lin, w.d)
            .expect("the fixture must compile as a block");
        let key = jit::direct::key_for(&w.cpu, w.lin, w.d).expect("a key for the fixture block");
        assert!(matches!(
            w.cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        let id = w
            .cpu
            .jit_direct
            .install(&compilation)
            .expect("install the fixture block");
        let block = w.cpu.jit_direct.block(id).expect("the block must be live");
        let used = w.cpu.jit_direct.segment_layout(id).unwrap().used_for_test();
        let chain_used = w.cpu.jit_direct.chain_requirement_used_for_test(id);
        NativeBuild {
            w,
            block,
            instructions: compilation.span.instructions,
            interpret_one_slots: compilation.callout_interpret_one_slots,
            used,
            chain_used,
        }
    }

    struct NativeBuild {
        w: World,
        block: jit::direct::CompiledBlock,
        instructions: u8,
        interpret_one_slots: u8,
        used: u8,
        chain_used: u8,
    }

    fn run_pair(code: &[u8], starts: &[u32], s: Setup, rows: Option<u8>) -> Pair {
        let mut interp_world = build(code, s);
        arm(&mut interp_world, s);
        drive(&mut interp_world.cpu, &mut interp_world.bus);

        let NativeBuild {
            mut w,
            block,
            instructions,
            interpret_one_slots,
            used,
            chain_used,
        } = compile_native(code, starts, s, rows);
        arm(&mut w, s);
        let before = w.cpu.perf_counters().jit_direct_insns;
        assert!(
            w.cpu
                .try_run_direct_block_for_test(&mut w.bus, block)
                .expect("the fixture block must not stop the machine"),
            "the installed block must actually run"
        );
        let native_insns = w.cpu.perf_counters().jit_direct_insns - before;
        let exit_reason = w.cpu.jit_direct.last_side_exit_reason_for_test();
        drive(&mut w.cpu, &mut w.bus);
        let target = match s.mode {
            Mode::V86 => (V86_BASE + V86_TARGET) as usize,
            _ => POP_TARGET as usize,
        };
        Pair {
            interp: interp_world.cpu,
            interp_bus: interp_world.bus,
            native: w.cpu,
            native_bus: w.bus,
            native_insns,
            exit_reason,
            instructions,
            interpret_one_slots,
            used,
            chain_used,
            target,
        }
    }

    /// The native leg and the interpreted leg end in the same architectural state.
    fn agree(pair: &mut Pair) {
        pair.native.materialize_flags();
        pair.interp.materialize_flags();
        assert_eq!(
            crate::tests::settled_registers(&pair.native),
            crate::tests::settled_registers(&pair.interp),
            "registers or EIP differ"
        );
        assert_eq!(pair.native.eflags(), pair.interp.eflags(), "EFLAGS");
        assert_eq!(
            pair.native_bus.memory, pair.interp_bus.memory,
            "guest RAM differs"
        );
        assert_eq!(
            pair.native.elapsed_clocks, pair.interp.elapsed_clocks,
            "guest clocks differ"
        );
        assert_eq!(
            pair.native.perf_counters().instructions,
            pair.interp.perf_counters().instructions,
            "retirement counts differ"
        );
    }

    fn stored(pair: &Pair) -> u16 {
        u16::from_le_bytes(
            pair.native_bus.memory[pair.target..pair.target + 2]
                .try_into()
                .unwrap(),
        )
    }

    /// `inc cx; <instructions>; inc cx; hlt` and the start offset of every instruction.
    fn program(instructions: &[Vec<u8>]) -> (Vec<u8>, Vec<u32>) {
        let mut code = vec![0x41];
        let mut starts = vec![0];
        for instruction in instructions {
            starts.push(code.len() as u32);
            code.extend_from_slice(instruction);
        }
        starts.push(code.len() as u32);
        code.push(0x41);
        code.push(0xF4);
        (code, starts)
    }

    fn store_sreg(mode: Mode, reg: u8, prefix: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if prefix {
            bytes.push(0x66);
        }
        bytes.push(0x8C);
        bytes.push((if mode.is_32() { 0x03 } else { 0x07 }) | (reg << 3));
        bytes
    }

    const MODES: [Mode; 4] = [Mode::Real, Mode::Pm16, Mode::Pm32, Mode::V86];

    // -----------------------------------------------------------------------------------------
    // Row 1: native MOV [mem16], Sreg.
    // -----------------------------------------------------------------------------------------

    /// Every Sreg, with and without 0x66, in all four modes. The call-out is gone, the store is
    /// native, and the stored word is the segment's selector.
    ///
    /// MUTATION: return `None` from `mov_rm_sreg_store_kind` and the slot count reads one.
    #[test]
    fn native_mov_rm16_sreg_matches_the_interpreter_for_every_segment() {
        for mode in MODES {
            for reg in 0..=5u8 {
                for prefix in [false, true] {
                    let (code, starts) = program(&[store_sreg(mode, reg, prefix)]);
                    let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
                    let what = format!("{mode:?} reg {reg} prefix {prefix}");
                    assert_eq!(pair.interpret_one_slots, 0, "{what}: still a call-out");
                    assert_eq!(pair.instructions, 3, "{what}: the block ended early");
                    agree(&mut pair);
                    let segment = [
                        SegmentIndex::Es,
                        SegmentIndex::Cs,
                        SegmentIndex::Ss,
                        SegmentIndex::Ds,
                        SegmentIndex::Fs,
                        SegmentIndex::Gs,
                    ][usize::from(reg)];
                    // A V86 leg ends in the monitor, which has cleared the guest segments.
                    let wanted = if mode == Mode::V86 {
                        [0x0A00, 0x0A00, 0x0900, 0x0A00, 0x5678, 0x9abc][usize::from(reg)]
                    } else {
                        pair.interp.registers.segment(segment).selector
                    };
                    assert_eq!(stored(&pair), wanted, "{what}: wrong selector");
                    if mode != Mode::V86 {
                        assert_eq!(pair.exit_reason, None, "{what}: side exit");
                        assert_eq!(pair.native_insns, 3, "{what}: native count");
                        let stalls = pair.native.direct_stall_snapshot();
                        assert_eq!(stalls.callout_interpret_one_executed, 0, "{what}");
                        assert_eq!(row_counts(&pair.native, "0x8c_mov_rm_sreg").executed, 0);
                    }
                }
            }
        }
    }

    /// A null FS in protected mode stores selector zero.
    #[test]
    fn native_mov_rm16_sreg_stores_a_null_fs() {
        fn null_fs(cpu: &mut CpuGsw, _: &mut TestBus) {
            cpu.registers.set_segment(
                SegmentIndex::Fs,
                SegmentRegister {
                    selector: 0,
                    base: 0,
                    limit: 0,
                    access: 0,
                    default_size_32: false,
                },
            );
        }
        for mode in [Mode::Pm16, Mode::Pm32] {
            let (code, starts) = program(&[store_sreg(mode, 4, false)]);
            let mut s = setup(mode);
            s.tweak = null_fs;
            let mut pair = run_pair(&code, &starts, s, Some(ALL));
            assert_eq!(pair.interpret_one_slots, 0, "{mode:?}");
            agree(&mut pair);
            assert_eq!(stored(&pair), 0, "{mode:?}");
            assert_eq!(pair.native_insns, 3, "{mode:?}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 2: a MOV Sreg call-out, then a store of that segment, in the same block.
    // -----------------------------------------------------------------------------------------

    /// The store reads the NEW selector. The `MOV FS` cell resumes or resyncs exactly as with the
    /// knob off, and the data segment selector read adds no pin and no resume mask bit.
    #[test]
    fn a_mov_sreg_call_out_then_a_native_store_sees_the_new_selector() {
        fn new_selector_real(cpu: &mut CpuGsw, _: &mut TestBus) {
            cpu.registers.set_edx(0x2222);
        }
        fn new_selector_pm(cpu: &mut CpuGsw, _: &mut TestBus) {
            cpu.registers.set_edx(u32::from(SEL_OTHER));
        }
        for mode in [Mode::Real, Mode::Pm16, Mode::Pm32] {
            // mov fs, dx; mov [bx], fs
            let (code, starts) = program(&[vec![0x8E, 0xE2], store_sreg(mode, 4, false)]);
            let mut s = setup(mode);
            s.tweak = if mode == Mode::Real {
                new_selector_real
            } else {
                new_selector_pm
            };
            let mut off = run_pair(&code, &starts, s, None);
            let mut on = run_pair(&code, &starts, s, Some(jit::direct::NATIVE_ROW_MOV_RM_SREG));
            agree(&mut off);
            agree(&mut on);
            let wanted = if mode == Mode::Real {
                0x2222
            } else {
                SEL_OTHER
            };
            assert_eq!(
                stored(&on),
                wanted,
                "{mode:?}: the store read a stale selector"
            );
            assert_eq!(stored(&off), wanted, "{mode:?}");
            // The row is native: only the MOV FS stays a call-out.
            assert_eq!(off.interpret_one_slots, 2, "{mode:?}");
            assert_eq!(on.interpret_one_slots, 1, "{mode:?}");
            // FS is the selector source, so neither the entry pin nor the chain requirement may
            // name it. The store addresses through DS.
            let fs_bit = 1u8 << SegmentIndex::Fs.index();
            assert_eq!(on.used & fs_bit, 0, "{mode:?}: the block pinned FS");
            assert_eq!(
                on.chain_used & fs_bit,
                0,
                "{mode:?}: the chain requirement names FS"
            );
            let before = row_counts(&off.native, "0x8e_mov_sreg");
            let after = row_counts(&on.native, "0x8e_mov_sreg");
            assert_eq!(
                (before.executed, before.resync, before.resync_fault),
                (after.executed, after.resync, after.resync_fault),
                "{mode:?}: the MOV FS cell behaves differently with the item on"
            );
            let off_stalls = off.native.direct_stall_snapshot();
            let on_stalls = on.native.direct_stall_snapshot();
            assert_eq!(
                off_stalls.callout_interpret_one_demoted,
                on_stalls.callout_interpret_one_demoted
            );
            assert_eq!(
                off_stalls.callout_interpret_one_resync,
                on_stalls.callout_interpret_one_resync
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 3: CS bakes a selector.
    // -----------------------------------------------------------------------------------------

    /// `MOV [x], CS` reports that the block bakes CS. `MOV [x], FS` does not.
    #[test]
    fn a_cs_store_marks_the_block_as_baking_cs() {
        for mode in [Mode::Real, Mode::Pm32] {
            let (code, starts) = program(&[store_sreg(mode, 1, false)]);
            let pair = run_pair(&code, &starts, setup(mode), Some(ALL));
            assert_ne!(
                pair.chain_used & jit::direct::BAKES_CS_BIT,
                0,
                "{mode:?}: BAKES_CS_BIT missing"
            );
            let (code, starts) = program(&[store_sreg(mode, 4, false)]);
            let pair = run_pair(&code, &starts, setup(mode), Some(ALL));
            assert_eq!(
                pair.chain_used & jit::direct::BAKES_CS_BIT,
                0,
                "{mode:?}: a live FS store must not bake CS"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 4: a store into the block's own code page.
    // -----------------------------------------------------------------------------------------

    /// `mov [ENTRY+0x20], ds` writes into the page the block is on. The result matches the
    /// interpreter, and the native leg takes the code-watch exit.
    #[test]
    fn a_selector_store_into_the_blocks_own_page_matches_the_interpreter() {
        // mov [0x120], ds in 16-bit code: 8C 1E 20 01. The 32-bit form is 8C 1D disp32.
        for mode in [Mode::Real, Mode::Pm16, Mode::Pm32] {
            let store = if mode.is_32() {
                vec![0x8C, 0x1D, 0x20, 0x01, 0, 0]
            } else {
                vec![0x8C, 0x1E, 0x20, 0x01]
            };
            let (mut code, starts) = program(&[store]);
            code.resize(0x40, 0x90);
            let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
            agree(&mut pair);
            assert_eq!(pair.interpret_one_slots, 0, "{mode:?}");
            // The watched page sends the store to the interpreter. The exit reason is the
            // code-watch family: CodeWatch from the guard, or UnavailableOrKind from the
            // one-lookup probe of a watched page.
            assert!(
                matches!(pair.exit_reason, Some(2 | 4)),
                "{mode:?}: the store into the code page must exit, got {:?}",
                pair.exit_reason
            );
            assert_eq!(
                pair.native_insns, 1,
                "{mode:?}: the store must not retire natively"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 5: reg fields 6 and 7 stay refused.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn mov_rm16_sreg_reg_fields_6_and_7_stay_refused_with_the_item_on() {
        for mode in [Mode::Real, Mode::Pm32] {
            for reg in [6u8, 7] {
                let padded = |reg| {
                    let mut code = vec![0x41, 0x41, 0x41];
                    code.extend(store_sreg(mode, reg, false));
                    code.extend_from_slice(&[0x41, 0xF4]);
                    code
                };
                let code = padded(reg);
                let starts = [0, 1, 2, 3];
                let s = setup(mode);
                let nb = compile_native(&code, &starts, s, Some(ALL));
                assert_eq!(
                    nb.instructions, 3,
                    "{mode:?} /{reg}: the block must end at the refused store"
                );
                assert_eq!(nb.interpret_one_slots, 0);
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Rows 6 to 11: CLI.
    // -----------------------------------------------------------------------------------------

    fn cli_program() -> (Vec<u8>, Vec<u32>) {
        program(&[vec![0xFA]])
    }

    /// CPL 0 to 3 against IOPL 0 to 3 in 32-bit protected mode. A site that faults at compile time
    /// keeps the call-out row. Every other site is native. Both match the interpreter, including
    /// the fault frame.
    #[test]
    fn native_cli_matches_the_interpreter_at_every_cpl_and_iopl() {
        let (code, starts) = cli_program();
        for cpl in 0..=3u8 {
            for iopl in 0..=3u8 {
                let mut pair = run_pair(&code, &starts, at(Mode::Pm32, cpl, iopl), Some(ALL));
                let what = format!("cpl {cpl} iopl {iopl}");
                agree(&mut pair);
                let faults = cpl > iopl;
                assert_eq!(
                    pair.interpret_one_slots,
                    u8::from(faults),
                    "{what}: wrong row choice"
                );
                if faults {
                    assert_eq!(pair.native.registers.eip, FAULT_HANDLER + 1, "{what}");
                    assert_eq!(pair.interp.registers.eip, FAULT_HANDLER + 1, "{what}");
                } else {
                    assert_eq!(pair.native.registers.eflags & FLAG_IF, 0, "{what}: IF set");
                    assert_eq!(pair.exit_reason, None, "{what}");
                    assert_eq!(pair.native_insns, 3, "{what}");
                    let stalls = pair.native.direct_stall_snapshot();
                    assert_eq!(stalls.callout_interpret_one_executed, 0, "{what}");
                }
            }
        }
    }

    /// A 16-bit protected-mode block and a real-mode block clear IF natively. Real mode emits no
    /// privilege test.
    #[test]
    fn native_cli_in_real_mode_and_16_bit_protected_mode() {
        let (code, starts) = cli_program();
        for mode in [Mode::Real, Mode::Pm16] {
            let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
            agree(&mut pair);
            assert_eq!(pair.interpret_one_slots, 0, "{mode:?}");
            assert_eq!(pair.native.registers.eflags & FLAG_IF, 0, "{mode:?}");
            assert_eq!(pair.native_insns, 3, "{mode:?}");
        }
    }

    /// V86 at IOPL 3 clears IF natively. V86 at IOPL 0 keeps the call-out row, which delivers the
    /// #GP.
    #[test]
    fn native_cli_in_v86() {
        let (code, starts) = cli_program();
        let mut pair = run_pair(&code, &starts, at(Mode::V86, 3, 3), Some(ALL));
        agree(&mut pair);
        assert_eq!(pair.interpret_one_slots, 0);
        assert_eq!(pair.native.registers.eflags & FLAG_IF, 0);
        assert_eq!(pair.native_insns, 3);
        assert!(!pair.native.is_v86_mode() || pair.native.registers.eflags & FLAG_IF == 0);

        let mut pair = run_pair(&code, &starts, at(Mode::V86, 3, 0), Some(ALL));
        agree(&mut pair);
        assert_eq!(
            pair.interpret_one_slots, 1,
            "a V86 CLI that faults at compile time must keep the call-out row"
        );
        assert!(
            !pair.native.is_v86_mode(),
            "the #GP must have reached the monitor"
        );
    }

    /// Row 11: the call-out site of a V86 CLI at IOPL 0 is governed, not looped natively. The
    /// block carries no native CLI, so no native side exit can repeat.
    #[test]
    fn a_faulting_v86_cli_site_stays_governed_by_the_call_out_row() {
        let (code, starts) = cli_program();
        let pair = run_pair(&code, &starts, at(Mode::V86, 3, 0), Some(ALL));
        assert_eq!(pair.interpret_one_slots, 1);
        assert_eq!(
            pair.exit_reason,
            Some(jit::direct::SideExitReason::CallOutResyncFault as u32),
            "the call-out row must deliver the fault"
        );
        let counts = row_counts(&pair.native, "0xfa_cli");
        assert_eq!(
            (counts.executed, counts.resync_fault),
            (1, 1),
            "the fault must land on the CLI row"
        );
    }

    /// Row 7: a block compiled at CPL 0 and entered at CPL 1 or 2 with IOPL 0 takes the run-time
    /// check. The block keeps its CS record: entry at CPL 3 is refused by the dispatcher itself,
    /// because the block bakes the CPL 3 memory bit, so ring 1 and ring 2 are the cases that reach
    /// the native CLI with a changed CPL.
    ///
    /// MUTATION: delete the `emit_cli_privilege_check` call and the native leg clears IF where the
    /// interpreter faults.
    #[test]
    fn a_block_compiled_at_cpl_0_faults_when_entered_at_a_higher_cpl() {
        let (code, starts) = cli_program();
        for cpl in [1u8, 2] {
            let mut s = at(Mode::Pm32, cpl, 0);
            s.compile_cpl = 0;
            s.compile_iopl = 0;
            s.cs_follows_cpl = false;
            let mut pair = run_pair(&code, &starts, s, Some(ALL));
            assert_eq!(
                pair.interpret_one_slots, 0,
                "cpl {cpl}: the block must hold a native CLI"
            );
            assert_eq!(
                pair.exit_reason,
                Some(jit::direct::SideExitReason::Permission as u32),
                "cpl {cpl}: the run-time check must side exit"
            );
            assert_eq!(
                pair.native_insns, 1,
                "cpl {cpl}: the CLI must not retire natively"
            );
            agree(&mut pair);
            assert_eq!(pair.native.registers.eip, FAULT_HANDLER + 1, "cpl {cpl}");
        }
    }

    /// Row 8: `popf` at CPL 0 changes IOPL, then a native CLI. The shadow must hold the new IOPL
    /// and IF must end clear. IOPL can only change at CPL 0, where CLI cannot fault, so this checks
    /// the shadow reload and not a new fault.
    #[test]
    fn a_popf_that_changes_iopl_before_a_native_cli_matches_the_interpreter() {
        fn raise(_: &mut CpuGsw, bus: &mut TestBus) {
            bus.memory[STACK_TOP as usize..STACK_TOP as usize + 4]
                .copy_from_slice(&0x0000_3202u32.to_le_bytes());
        }
        fn lower(cpu: &mut CpuGsw, bus: &mut TestBus) {
            cpu.registers.eflags |= 0x3000;
            bus.memory[STACK_TOP as usize..STACK_TOP as usize + 4]
                .copy_from_slice(&0x0000_0202u32.to_le_bytes());
        }
        let (code, starts) = program(&[vec![0x66, 0x9D], vec![0xFA]]);
        for (tweak, name) in [
            (raise as fn(&mut CpuGsw, &mut TestBus), "raise"),
            (lower, "lower"),
        ] {
            let mut s = setup(Mode::Pm32);
            s.tweak = tweak;
            let mut pair = run_pair(&code, &starts, s, Some(ALL));
            agree(&mut pair);
            assert_eq!(pair.native.registers.eflags & FLAG_IF, 0, "{name}");
            let want_iopl = if name == "raise" { 3 } else { 0 };
            assert_eq!(
                (pair.native.registers.eflags >> 12) & 3,
                want_iopl,
                "{name}"
            );
        }
    }

    /// Row 9: CLI, then an arithmetic instruction, then the block exit. IF stays clear.
    ///
    /// MUTATION: clear IF in the RBP shadow only, or in the in-memory EFLAGS only, and the final
    /// EFLAGS differ from the interpreter's.
    #[test]
    fn native_cli_survives_a_following_arithmetic_slot_and_the_block_exit() {
        // cli; add ecx, 1 (83 C1 01); cmp ecx, 7 (83 F9 07)
        let (code, starts) = program(&[vec![0xFA], vec![0x83, 0xC1, 0x01], vec![0x83, 0xF9, 0x07]]);
        for mode in [Mode::Pm32, Mode::Real] {
            let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
            agree(&mut pair);
            assert_eq!(pair.native.registers.eflags & FLAG_IF, 0, "{mode:?}");
            assert_eq!(pair.interpret_one_slots, 0, "{mode:?}");
        }
    }

    /// CLI followed by readers of the in-memory EFLAGS: a `pushf` call-out and a side exit taken
    /// by a store into the block's own page. Both must see IF clear.
    ///
    /// MUTATION: clear IF only in the RBP shadow, and a reader that goes to memory sees IF set.
    #[test]
    fn native_cli_is_visible_to_readers_of_the_memory_copy() {
        for mode in [Mode::Real, Mode::Pm32] {
            let store = if mode.is_32() {
                vec![0x8C, 0x1D, 0x20, 0x01, 0, 0]
            } else {
                vec![0x8C, 0x1E, 0x20, 0x01]
            };
            // The word PUSHF is a call-out only with a 16-bit stack, so it runs in real mode.
            let mut cases = vec![("side exit", vec![vec![0xFA], store.clone()])];
            if mode == Mode::Real {
                cases.push(("pushf", vec![vec![0xFA], vec![0x9C]]));
                cases.push((
                    "pushf after a native slot",
                    vec![vec![0xFA], vec![0x41], vec![0x9C]],
                ));
            }
            for (name, rows) in cases {
                let (mut code, starts) = program(&rows);
                code.resize(0x40, 0x90);
                let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
                agree(&mut pair);
                assert_eq!(pair.native.registers.eflags & FLAG_IF, 0, "{mode:?} {name}");
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 10: interrupt edges.
    // -----------------------------------------------------------------------------------------

    /// The existing `an_ss_load_does_not_lose_an_irq_to_a_later_cli_slot` program with the item on.
    #[test]
    fn an_ss_load_does_not_lose_an_irq_to_a_later_native_cli() {
        fn pend(_: &mut CpuGsw, bus: &mut TestBus) {
            bus.pending_irq = Some(0x08);
        }
        let _guard = RowsGuard;
        jit::direct::set_native_callout_rows_for_test(Some(ALL));
        // mov ax,0x1111; mov ss,cx; nop; cli; hlt
        let code = [0xB8, 0x11, 0x11, 0x8E, 0xD1, 0x90, 0xFA, 0xF4];
        let starts = [0, 3, 5, 6, 7];
        let (mut cpu, mut bus, native_insns, _) = run_block_to_boundary(&code, &starts, pend);
        assert_eq!(native_insns, 2, "the block must stop at the SS load");
        assert_eq!(
            native_insns + instructions_until_the_vector_lands(&mut cpu, &mut bus),
            3,
            "the vector must land on the instruction after the SS load, before the CLI"
        );
    }

    /// `sti; cli` with an interrupt pending: both legs end in the same state, IF clear.
    #[test]
    fn sti_then_a_native_cli_matches_the_interpreter() {
        fn pend_with_if_clear(cpu: &mut CpuGsw, bus: &mut TestBus) {
            cpu.registers.eflags = 0x002;
            bus.pending_irq = Some(0x08);
        }
        let (code, starts) = program(&[vec![0xFB], vec![0xFA]]);
        let mut s = setup(Mode::Real);
        s.tweak = pend_with_if_clear;
        let mut pair = run_pair(&code, &starts, s, Some(ALL));
        agree(&mut pair);
        assert_eq!(pair.native.registers.eflags & FLAG_IF, 0);

        // And with IF already set, so the STI is a no-op edge and the CLI closes the window.
        fn pend(_: &mut CpuGsw, bus: &mut TestBus) {
            bus.pending_irq = Some(0x08);
        }
        s.tweak = pend;
        let mut pair = run_pair(&code, &starts, s, Some(ALL));
        agree(&mut pair);
    }

    // -----------------------------------------------------------------------------------------
    // Row 12: XCHG register forms.
    // -----------------------------------------------------------------------------------------

    fn seed_registers(cpu: &mut CpuGsw, _: &mut TestBus) {
        cpu.registers.set_eax(0x1111_aaa1);
        cpu.registers.set_ecx(0x2222_bbb2);
        cpu.registers.set_edx(0x3333_ccc3);
        cpu.registers.set_ebx(POP_TARGET);
        cpu.registers.set_esp(0x4444_ddd4 & !3);
        cpu.registers.set_ebp(0x5555_eee5);
        cpu.registers.set_esi(0x6666_fff6);
        cpu.registers.set_edi(0x7777_0007);
        cpu.registers.eflags = 0x202 | crate::FLAG_CF | crate::FLAG_ZF | crate::FLAG_OF;
    }

    fn xchg_programs() -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        for r in 1..=7u8 {
            out.push((format!("xchg ax,r{r}"), vec![0x90 + r]));
        }
        for rm in 0..8u8 {
            for reg in 0..8u8 {
                out.push((
                    format!("87 /r rm{rm} reg{reg}"),
                    vec![0x87, 0xC0 | (reg << 3) | rm],
                ));
                out.push((
                    format!("86 /r rm{rm} reg{reg}"),
                    vec![0x86, 0xC0 | (reg << 3) | rm],
                ));
            }
        }
        out
    }

    /// Every pair class in 16-bit and 32-bit code, including a register with itself and AL with
    /// AH. The upper bits, the other bytes, and the flags stay as the interpreter leaves them.
    /// ESP forms are native, because every guest register has a home and later slots reload ESP
    /// from its home.
    #[test]
    fn native_xchg_reg_matches_the_interpreter() {
        for mode in [Mode::Real, Mode::Pm32] {
            for (name, bytes) in xchg_programs() {
                let (code, starts) = program(std::slice::from_ref(&bytes));
                let mut s = setup(mode);
                s.tweak = seed_registers;
                let mut pair = run_pair(&code, &starts, s, Some(ALL));
                agree(&mut pair);
                assert_eq!(
                    pair.interpret_one_slots, 0,
                    "{mode:?} {name}: still a call-out"
                );
                assert_eq!(pair.native_insns, 3, "{mode:?} {name}");
            }
        }
    }

    /// The 0x66 prefix flips the width: a word exchange in 32-bit code and a dword exchange in
    /// 16-bit code.
    #[test]
    fn native_xchg_reg_honours_the_operand_size_prefix() {
        for mode in [Mode::Real, Mode::Pm32] {
            for (name, bytes) in [
                ("66 93", vec![0x66, 0x93]),
                ("66 87 CA", vec![0x66, 0x87, 0xCA]),
            ] {
                let (code, starts) = program(&[bytes]);
                let mut s = setup(mode);
                s.tweak = seed_registers;
                let mut pair = run_pair(&code, &starts, s, Some(ALL));
                agree(&mut pair);
                assert_eq!(pair.interpret_one_slots, 0, "{mode:?} {name}");
            }
        }
    }

    /// The memory forms stay call-outs.
    #[test]
    fn xchg_memory_forms_stay_call_outs() {
        for mode in [Mode::Real, Mode::Pm32] {
            let rm = if mode.is_32() { 0x03 } else { 0x07 };
            for opcode in [0x86u8, 0x87] {
                let (code, starts) = program(&[vec![opcode, rm]]);
                let mut pair = run_pair(&code, &starts, setup(mode), Some(ALL));
                agree(&mut pair);
                assert_eq!(pair.interpret_one_slots, 1, "{mode:?} {opcode:02x}");
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 13: the knob off.
    // -----------------------------------------------------------------------------------------

    /// With the knob off the three rows are call-outs, the executed counters move as before, and
    /// the block carries its pre-change shape. There is no pre-change binary to compare bytes
    /// against, so this compares the compiled slot counts and the full differential result, and
    /// shows that turning the knob on changes the block.
    #[test]
    fn with_the_knob_off_the_rows_stay_call_outs() {
        let (code, starts) = program(&[store_sreg(Mode::Real, 3, false), vec![0xFA], vec![0x93]]);
        for rows in [None, Some(0)] {
            let mut pair = run_pair(&code, &starts, setup(Mode::Real), rows);
            agree(&mut pair);
            assert_eq!(pair.interpret_one_slots, 3, "{rows:?}");
            let stalls = pair.native.direct_stall_snapshot();
            assert_eq!(stalls.callout_interpret_one_executed, 3, "{rows:?}");
            assert_eq!(pair.native_insns, 5, "{rows:?}");
        }
        let nb_off = compile_native(&code, &starts, setup(Mode::Real), None);
        let nb_zero = compile_native(&code, &starts, setup(Mode::Real), Some(0));
        let nb_on = compile_native(&code, &starts, setup(Mode::Real), Some(ALL));
        let len_of = |nb: &NativeBuild| nb.w.cpu.jit_direct.total_live_code_len_for_test();
        assert_eq!(len_of(&nb_off), len_of(&nb_zero));
        assert_ne!(len_of(&nb_off), len_of(&nb_on));
        assert_eq!(nb_on.interpret_one_slots, 0);
    }

    /// Each item alone lowers only its own row.
    #[test]
    fn each_item_lowers_only_its_own_row() {
        let (code, starts) = program(&[store_sreg(Mode::Real, 3, false), vec![0xFA], vec![0x93]]);
        for (item, slots) in [
            (jit::direct::NATIVE_ROW_MOV_RM_SREG, 2),
            (jit::direct::NATIVE_ROW_CLI, 2),
            (jit::direct::NATIVE_ROW_XCHG_REG, 2),
        ] {
            let nb = compile_native(&code, &starts, setup(Mode::Real), Some(item));
            assert_eq!(nb.interpret_one_slots, slots, "item {item}");
        }
    }

    // -----------------------------------------------------------------------------------------
    // Row 14: the spelling table.
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_native_callout_rows_knob_spellings() {
        let parse =
            |value: &str| jit::direct::parse_native_callout_rows_for_test(Ok(value.to_string()));
        assert_eq!(
            jit::direct::parse_native_callout_rows_for_test(Err(std::env::VarError::NotPresent)),
            0
        );
        assert_eq!(parse(""), 0);
        assert_eq!(parse("0"), 0);
        assert_eq!(parse("1"), ALL);
        assert_eq!(parse("mov_rm_sreg"), jit::direct::NATIVE_ROW_MOV_RM_SREG);
        assert_eq!(parse("cli"), jit::direct::NATIVE_ROW_CLI);
        assert_eq!(parse("xchg_reg"), jit::direct::NATIVE_ROW_XCHG_REG);
        assert_eq!(
            parse("cli,xchg_reg"),
            jit::direct::NATIVE_ROW_CLI | jit::direct::NATIVE_ROW_XCHG_REG
        );
        assert_eq!(
            parse(" mov_rm_sreg , cli "),
            jit::direct::NATIVE_ROW_MOV_RM_SREG | jit::direct::NATIVE_ROW_CLI
        );
        for bad in ["2", "on", "cli,", "cli,nope", "CLI", "mov_rm_sreg;cli"] {
            let result = std::panic::catch_unwind(|| {
                jit::direct::parse_native_callout_rows_for_test(Ok(bad.to_string()))
            });
            assert!(result.is_err(), "{bad:?} must panic");
        }
    }
}
