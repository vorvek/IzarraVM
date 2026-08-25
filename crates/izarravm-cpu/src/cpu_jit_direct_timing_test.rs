// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn mode13_read_self_loop_respects_the_tight_native_deadline() {
    const ENTRY: u32 = 0x101;
    const MODE13: u32 = 0x000a_0000;

    let mut memory = vec![0; 0x000b_0000];
    memory[ENTRY as usize..ENTRY as usize + 11].copy_from_slice(&[
        0xa0, 0x00, 0x00, 0x0a, 0x00, // mov al,[0xa0000]
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf6, // jnz ENTRY
        0xf4, // hlt
    ]);
    memory[MODE13 as usize] = 0x5a;

    let mut native = fresh();
    let mut interp = fresh();
    make_data_segments_flat(&mut native);
    make_data_segments_flat(&mut interp);
    native.registers.eip = ENTRY;
    interp.registers.eip = ENTRY;
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        bus.uniform_native_fetches = true;
    }
    let starts = [ENTRY, ENTRY + 5, ENTRY + 8];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            MODE13,
            MODE13,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
    }
    let block = install_fixture_block(&mut native, ENTRY);
    assert!(block.is_self_loop());
    assert_eq!(block.byte_reads(), 1);
    assert_eq!(block.word_reads(), 0);
    assert_eq!(block.dword_reads(), 0);

    let (num, den) = level_timing(native.persona());
    let fp_core_upper = u64::from(block.weighted_fp_clocks())
        .saturating_add(u64::from(FP_TIMING_DEN) - 1)
        / u64::from(FP_TIMING_DEN);
    let scaled_core_upper = u64::from(block.raw_clocks())
        .saturating_add(fp_core_upper)
        .saturating_mul(u64::from(num))
        .saturating_add(u64::from(den) - 1)
        / u64::from(den);
    let ram_read_upper = native_bus.jit_data_cost_clocks(BusWidth::Byte);
    let mode13_read_upper = native_bus.jit_mode13_data_cost_clocks(BusWidth::Byte);
    assert!(mode13_read_upper > ram_read_upper);
    let fetch_upper = native_bus
        .jit_fetch_cost_clocks()
        .saturating_mul(u64::from(block.span().instructions));
    let ram_only_iteration_upper = scaled_core_upper.saturating_add(
        native_bus.jit_scale_bus_cost_upper(fetch_upper.saturating_add(ram_read_upper)),
    );
    let iteration_upper = scaled_core_upper.saturating_add(
        native_bus.jit_scale_bus_cost_upper(fetch_upper.saturating_add(mode13_read_upper)),
    );
    assert!(iteration_upper > ram_only_iteration_upper);

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_ecx(2);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();
    let start_registers = native.registers.clone();
    let start_eflags = native.eflags();
    let zero_budget_rejects = native.perf_counters().jit_direct_reject_zero_budget;

    assert!(
        !native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, iteration_upper)
            .unwrap()
    );
    assert_eq!(native.registers, start_registers);
    assert_eq!(native.eflags(), start_eflags);
    assert_eq!(native.elapsed_clocks, 0);
    assert_eq!(native.timing_rem, 0);
    assert_eq!(native_bus.trace.elapsed_clocks(), 0);
    assert_eq!(
        native.perf_counters().jit_direct_reject_zero_budget - zero_budget_rejects,
        1
    );

    let loads = native.perf_counters().jit_native_load_hits;
    assert!(
        native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, iteration_upper + 1,)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.registers.eip, ENTRY);
    assert_eq!(native.registers.ecx(), 1);
    assert_eq!(native.registers.eax() & 0xff, 0x5a);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.timing_rem, interp.timing_rem);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert!(
        native
            .elapsed_clocks
            .saturating_add(native_bus.trace.elapsed_clocks())
            < iteration_upper + 1
    );
    assert_eq!(native.perf_counters().jit_native_load_hits - loads, 1);
}

/// MOVZX and MOVSX, memory form. `signed` selects MOVSX, `word` the 16-bit source.
///
/// The instruction is `movzx/movsx ebx, byte/word [esi + target]`, with ESI held at zero by the
/// fixture so the effective address is `target` and can be pointed at either plain RAM or the
/// mode13 aperture. A base register rather than a bare disp32 because `[reg + disp32]` is the form
/// Quake's texture fetch actually uses. Slots 1 and 2 are register moves, so the block carries
/// exactly ONE memory access and the read counters below are unambiguous.
///
/// The original note here claimed a bare no-SIB disp32 decodes with scale 0 and is therefore
/// rejected by `direct_addr`. That is wrong: `decode` seeds `scale` to 1 and only overwrites it in
/// the SIB branch, so a no-SIB absolute is accepted. Measured directly while adding the IMUL
/// memory form, whose retired negative test had been resting on that belief.
fn movzx_case(signed: bool, word: bool, target: u32) -> Vec<u8> {
    let opcode: u8 = match (signed, word) {
        (false, false) => 0xb6,
        (false, true) => 0xb7,
        (true, false) => 0xbe,
        (true, true) => 0xbf,
    };
    // 0F <op> /r, mod=10 (disp32) rm=110 (esi), reg=ebx(3) -> modrm 0b10_011_110 = 0x9e.
    let mut code = vec![0x0f, opcode, 0x9e];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    code
}

/// THE REGISTRATION TEST. A new memory-bearing DirectKind defaults to zero in `byte_reads`,
/// `word_reads` and `dword_reads`, and to None in `read_segment`. Nothing in the emitted code
/// fails when that happens: the block just under-declares its own memory traffic, and the
/// divergence surfaces as a `raw_bus_clocks` mismatch hours into a timedemo. These assertions read
/// the counts straight off the compiled block, which is the cheapest place to catch it.
#[test]
fn movzx_memory_forms_declare_their_read_width() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    for (signed, word) in [(false, false), (false, true), (true, false), (true, true)] {
        let code = movzx_case(signed, word, TARGET);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
        decode_fixture(&mut cpu, &mut bus, &starts);
        map_direct_page(
            &mut cpu,
            &mut bus,
            TARGET,
            TARGET,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
        let block = install_fixture_block(&mut cpu, ENTRY);
        let label = format!("signed={signed} word={word}");
        assert_eq!(
            block.span().instructions,
            3,
            "{label}: whole block admitted"
        );
        assert_eq!(
            block.byte_reads(),
            u8::from(!word),
            "{label}: byte-read declaration"
        );
        assert_eq!(
            block.word_reads(),
            u8::from(word),
            "{label}: word-read declaration"
        );
        assert_eq!(block.dword_reads(), 0, "{label}: no dword read");
        // clocks(3) per interpreter arm, three slots, minus the two register moves at 2 each.
        assert_eq!(
            block.raw_clocks(),
            3 + 2 + 2,
            "{label}: charged core clocks"
        );
    }
}

/// Extension correctness and equal timing against the interpreter, on both a plain-RAM target and
/// the mode13 aperture. The mode13 half matters for more than coverage: an under-declared byte
/// read trips a debug_assert in the run loop there, which is a loud failure, whereas the RAM half
/// only shows up as a bus-clock difference.
#[test]
fn movzx_memory_forms_match_the_interpreter_and_its_bus_clocks() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    // 0x80 and 0x8000 have the source's high bit SET, so zero and sign extension disagree; 0x7f
    // and 0x7f00 have it clear, so they agree. A lowering that used movzx where movsx belongs
    // passes every non-negative seed, which is why both polarities are here.
    for (signed, word) in [(false, false), (false, true), (true, false), (true, true)] {
        for target in [RAM, MODE13] {
            for raw in [0x80u32, 0x7f, 0xff, 0x00, 0x8000, 0x7f00] {
                let code = movzx_case(signed, word, target);
                let mut memory = vec![0; 0x000b_0000];
                memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
                memory[target as usize] = raw as u8;
                memory[target as usize + 1] = (raw >> 8) as u8;

                let mut native = fresh();
                let mut interp = fresh();
                make_data_segments_flat(&mut native);
                make_data_segments_flat(&mut interp);
                native.registers.eip = ENTRY;
                interp.registers.eip = ENTRY;
                let mut native_bus = TestBus::with_memory(memory.clone());
                let mut interp_bus = TestBus::with_memory(memory);
                for bus in [&mut native_bus, &mut interp_bus] {
                    bus.direct_pages_enabled = true;
                    bus.direct_page_clocks = true;
                    bus.report_batch_clocks = true;
                    bus.uniform_native_fetches = true;
                }
                let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
                decode_fixture(&mut native, &mut native_bus, &starts);
                decode_fixture(&mut interp, &mut interp_bus, &starts);
                for (cpu, bus) in [
                    (&mut native, &mut native_bus),
                    (&mut interp, &mut interp_bus),
                ] {
                    map_direct_page(
                        cpu,
                        bus,
                        target,
                        target,
                        jit::fast_map::PagePermissions::UNPAGED,
                        true,
                        false,
                    );
                }
                let block = install_fixture_block(&mut native, ENTRY);

                for cpu in [&mut native, &mut interp] {
                    cpu.halted = false;
                    cpu.interrupt_shadow = false;
                    // A non-zero high half in EBX, so a lowering that merged into the low 8 or 16
                    // bits instead of writing all 32 is caught. That is exactly what the shared
                    // emit_write_gpr8 path in emit_load would produce.
                    cpu.registers.gpr.fill(0xdead_beef);
                    cpu.registers.set_esp(0xc000);
                    // ESI is the address base and must be zero for the effective address to be
                    // `target`. Set AFTER the fill, or it keeps the filler and the load reads a
                    // wild address. The destination EBX deliberately keeps its non-zero high half.
                    cpu.registers.set_esi(0);
                    cpu.registers.eflags = 0x202;
                    cpu.pending_flags = PendingFlags::default();
                    cpu.registers.eip = ENTRY;
                    cpu.elapsed_clocks = 0;
                    cpu.timing_rem = 0;
                    cpu.core_clocks_so_far = 0;
                }
                native_bus.trace = BusTrace::default();
                interp_bus.trace = BusTrace::default();

                let label =
                    format!("signed={signed} word={word} target={target:#x} raw={raw:#06x}");
                assert!(
                    native
                        .try_run_direct_block_for_test(&mut native_bus, block)
                        .unwrap(),
                    "{label}: must run natively"
                );
                for _ in 0..3 {
                    interp.cycle(&mut interp_bus).unwrap();
                }

                assert_eq!(
                    crate::tests::settled_registers(&native),
                    crate::tests::settled_registers(&interp),
                    "{label}: registers"
                );
                assert_eq!(native.eflags(), interp.eflags(), "{label}: eflags");
                assert_eq!(
                    native.elapsed_clocks, interp.elapsed_clocks,
                    "{label}: core clocks"
                );
                assert_eq!(
                    native_bus.trace.elapsed_clocks(),
                    interp_bus.trace.elapsed_clocks(),
                    "{label}: BUS clocks, the registration check"
                );

                // Pin the concrete extension, not only agreement: if both sides were wrong in the
                // same direction the comparison above would still pass.
                let source = if word { raw & 0xffff } else { raw & 0xff };
                let expected = match (signed, word) {
                    (false, _) => source,
                    (true, false) => source as u8 as i8 as i32 as u32,
                    (true, true) => source as u16 as i16 as i32 as u32,
                };
                assert_eq!(native.registers.ebx(), expected, "{label}: extended value");
            }
        }
    }
}

/// The 66-prefixed MOVZX/MOVSX forms are LOWERED as of the rejected-row campaign's Slice 3.
///
/// This test was `movzx_word_operand_forms_remain_interpreter_only` and asserted the opposite. The
/// reason it gave for the refusal was correct and is worth keeping verbatim, because it is now the
/// specification of the fix rather than of the gate: "`write_gpr_sized` at Word MERGES into the low
/// 16 bits instead of replacing all 32, so lowering one as the 32-bit form would clobber the
/// destination's high half." `DirectKind`'s `dst_width` field expresses that merge, and
/// `cpu_jit_word_memory_test.rs` is the differential row that proves the emitted code performs it.
/// What is left HERE is the admission pin: all six encodings must join the block.
///
/// The doom census ranks `0x0FB6` memory word at 1,442,795 exits, quake at 31,216. The REGISTER
/// rows are in the same arm and are admitted with it; neither fixture measures one, so they are
/// pinned here and nowhere else.
#[test]
fn movzx_word_operand_forms_are_lowered() {
    const ENTRY: u32 = 0x101;
    for code in [
        vec![0x66u8, 0x0f, 0xb6, 0xd8], // 66 MOVZX bx, al: REGISTER form
        vec![0x66, 0x0f, 0xb7, 0xd8],   // 66 MOVZX bx, ax
        vec![0x66, 0x0f, 0xbe, 0xd8],   // 66 MOVSX bx, al
        vec![0x66, 0x0f, 0xbf, 0xd8],   // 66 MOVSX bx, ax
        vec![0x66, 0x0f, 0xb6, 0x1d, 0x00, 0x00, 0x03, 0x00],
        vec![0x66, 0x0f, 0xbf, 0x1d, 0x00, 0x00, 0x03, 0x00],
    ] {
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut block = code.clone();
        block.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
        memory[ENTRY as usize..ENTRY as usize + block.len()].copy_from_slice(&block);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        // Three warmed starts, not one. Warming only the entry line makes slot 1 miss, the walk
        // stops at Retry, and the fewer-than-three-slots gate returns the same None a real reject
        // would, so the assertion below would pass whether or not the opcode was lowered.
        let starts = [
            ENTRY,
            ENTRY + code.len() as u32,
            ENTRY + code.len() as u32 + 2,
        ];
        decode_fixture(&mut cpu, &mut bus, &starts);
        // Map the target page. WITHOUT this the compile refuses for want of a direct-page
        // mapping and the assertion below passes for a reason that has nothing to do with the
        // opcode, which is the vacuous-negative failure class this codebase has already been
        // bitten by. The positive test above is what proves the mapping is sufficient.
        map_direct_page(
            &mut cpu,
            &mut bus,
            0x0003_0000,
            0x0003_0000,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
        let compilation = jit::direct::compile(&mut cpu, ENTRY, true)
            .unwrap_or_else(|| panic!("{code:02x?} must be lowered"));
        assert_eq!(
            compilation.span.instructions, 3,
            "{code:02x?}: the word form must join the block rather than end it"
        );
        // The memory rows read ONE byte or ONE word, never a dword: `width` is the SOURCE width
        // and comes from the sub-opcode, so a future edit passing `operand_width` there instead of
        // to `dst_width` shows up as a dword read here.
        assert_eq!(compilation.dword_reads, 0, "{code:02x?}: dword reads");
        assert_eq!(compilation.dword_stores, 0, "{code:02x?}: dword stores");
        assert_eq!(compilation.word_stores, 0, "{code:02x?}: word stores");
    }
}

#[test]
fn movzx_memory_forms_are_lowered() {
    // The positive half of the guard above, and the ONLY test that can detect the classify arm
    // being placed among the u8-keyed arms, where the `u8::try_from(insn.opcode)` truncation makes
    // it unreachable for a two-byte opcode. Every negative assertion passes when that happens.
    const ENTRY: u32 = 0x101;
    for (signed, word) in [(false, false), (false, true), (true, false), (true, true)] {
        let code = movzx_case(signed, word, 0x0003_0000);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
        decode_fixture(&mut cpu, &mut bus, &starts);
        map_direct_page(
            &mut cpu,
            &mut bus,
            0x0003_0000,
            0x0003_0000,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
        let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
        let instructions = outcome
            .is_some()
            .then(|| outcome.unwrap().span.instructions);
        assert_eq!(
            instructions,
            Some(3),
            "signed={signed} word={word} must admit and carry the whole block"
        );
    }
}

/// IMUL r32, r/m32, memory form: `imul ebx, [esi + target]`.
///
/// ESI is held at zero by every fixture below, so the effective address is `target` and can be
/// aimed at plain RAM or at the mode13 aperture. A base register rather than a bare disp32 because
/// `[reg + disp32]` is the form Quake actually uses, and because it lets the aliasing fixture point
/// the base at the destination. Slots 1 and 2 are register moves that touch neither flags nor ESI,
/// so the block carries exactly ONE memory access and one flag write.
///
/// 0F AF /r, mod=10 (disp32) rm=110 (esi), reg=011 (ebx) -> modrm 0b10_011_110 = 0x9e.
fn imul_mem_case(target: u32) -> Vec<u8> {
    let mut code = vec![0x0fu8, 0xaf, 0x9e];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    code
}

/// THE REGISTRATION TEST. A new memory-bearing DirectKind defaults to zero in `dword_reads`, to
/// false in `has_dword_read` and to None in `read_segment`, and nothing in the emitted code fails
/// when that happens: the block under-declares its own traffic and the divergence surfaces as a
/// `raw_bus_clocks` mismatch hours into a timedemo. `read_segment` is worse than bookkeeping, so
/// it gets a release-meaningful assertion of its own below rather than resting on the
/// `debug_assert` inside `SegmentLayout::descriptor`.
#[test]
fn imul_memory_form_declares_its_read_and_its_segment() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let code = imul_mem_case(TARGET);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let block = install_fixture_block(&mut cpu, ENTRY);

    assert_eq!(block.span().instructions, 3, "whole block admitted");
    assert_eq!(block.byte_reads(), 0, "no byte read");
    assert_eq!(block.word_reads(), 0, "no word read");
    assert_eq!(block.dword_reads(), 1, "dword-read declaration");
    // clocks(9) for the IMUL per the interpreter's 0x0faf arm, plus 2 each for the two moves. The
    // DirectKind default is 2, so a missing raw_clocks arm shows up here as 2 + 2 + 2.
    assert_eq!(block.raw_clocks(), 9 + 2 + 2, "charged core clocks");
    // `has_dword_read` feeds the block's `has_wide_accesses`, which run.rs consults before running
    // a block while #AC is armed at CPL 3. Nothing else in this file reads it.
    assert!(block.has_wide_accesses(), "wide-access declaration");

    // THE read_segment ASSERTION, and the reason it is written this way. Defaulting to None keeps
    // DS out of the block's SegmentLayout mask, and `data_matches` SKIPS every segment outside
    // that mask. So a cached block would keep matching after the guest reloads DS and would go on
    // reading through a stale base. That is a wrong-memory-read bug, not lost bookkeeping, and it
    // is invisible to the debug_assert in `SegmentLayout::descriptor` in a release build.
    assert!(
        cpu.jit_direct
            .segment_layout(block.id())
            .expect("live block layout")
            .data_matches(&cpu),
        "the block must match the segments it was compiled under"
    );
    let mut reloaded = cpu.registers.segment(SegmentIndex::Ds);
    reloaded.base = 0x1_0000;
    cpu.registers.set_segment(SegmentIndex::Ds, reloaded);
    assert!(
        !cpu.jit_direct
            .segment_layout(block.id())
            .expect("live block layout")
            .data_matches(&cpu),
        "reloading DS must invalidate a block that reads through DS"
    );
}

/// THE WIDTH REGISTRATION TEST for the x87 control-word pair, and it has to exist because
/// `raw_bus_clocks` CANNOT see the width. `BusCycle::clocks_for` underscores its width parameter
/// and returns `2 + wait_states` for Byte, Word and Dword alike, so a word access charged as a
/// dword produces byte-identical bus clocks, core clocks and master ticks. The end-to-end
/// differential batteries are blind to it. These four accessors are the only direct evidence.
///
/// The block deliberately carries BOTH widths. With only the control word in it, dropping the
/// x87 word arm would leave every counter at zero, `map_bases` would be None and the emitter's
/// `memory.map.expect(..)` would panic, which is loud. Mixed with a dword read the same mistake
/// is silent, and silent is the case worth testing.
///
/// `fldcw word [esi+0x30000]`  d9 /5 mod=10 rm=110 -> modrm 0b10_101_110 = 0xae
/// `fnstcw word [esi+0x30000]` d9 /7 mod=10 rm=110 -> modrm 0b10_111_110 = 0xbe
/// `mov edx,[esi+0x30004]`     8b /r mod=10 rm=110 reg=010 -> modrm 0b10_010_110 = 0x96
fn control_word_case(store: bool, target: u32) -> Vec<u8> {
    let mut code = vec![0xd9u8, if store { 0xbe } else { 0xae }];
    code.extend_from_slice(&target.to_le_bytes());
    code.push(0x8b);
    code.push(0x96);
    code.extend_from_slice(&(target + 4).to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
    code
}

#[test]
fn control_word_forms_declare_a_word_access_and_their_segment() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    for store in [false, true] {
        let code = control_word_case(store, TARGET);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        let starts = [ENTRY, ENTRY + 6, ENTRY + 12];
        decode_fixture(&mut cpu, &mut bus, &starts);
        map_direct_page(
            &mut cpu,
            &mut bus,
            TARGET,
            TARGET,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
        let block = install_fixture_block(&mut cpu, ENTRY);
        let label = if store { "fnstcw" } else { "fldcw" };

        assert_eq!(
            block.span().instructions,
            3,
            "{label}: whole block admitted"
        );
        assert_eq!(block.byte_reads(), 0, "{label}: no byte read");
        assert_eq!(
            block.word_reads(),
            u8::from(!store),
            "{label}: word-read declaration"
        );
        assert_eq!(block.dword_reads(), 1, "{label}: the MOV's dword read only");
        assert_eq!(block.byte_stores(), 0, "{label}: no byte store");
        assert_eq!(
            block.word_stores(),
            u8::from(store),
            "{label}: word-store declaration"
        );
        assert_eq!(block.dword_stores(), 0, "{label}: no dword store");
        // 2 for the MOV and 2 for the register move. The x87 slot contributes ZERO here: its cost
        // is `weighted_fp_clocks`, and an added `Self::X87` arm in `DirectKind::raw_clocks` would
        // double-charge it.
        assert_eq!(block.raw_clocks(), 2 + 2, "{label}: charged core clocks");

        // read_segment / write_segment. Defaulting to None keeps DS out of the block's
        // SegmentLayout mask, and `data_matches` SKIPS every segment outside that mask, so a
        // cached block would keep matching after a guest DS reload and read or write through a
        // stale base. The `debug_assert` in `SegmentLayout::descriptor` is absent from a release
        // build, so the assertion is made against the live descriptor instead.
        assert!(
            cpu.jit_direct
                .segment_layout(block.id())
                .expect("live block layout")
                .data_matches(&cpu),
            "{label}: compiled state"
        );
        let mut reloaded = cpu.registers.segment(SegmentIndex::Ds);
        reloaded.base = 0x1_0000;
        cpu.registers.set_segment(SegmentIndex::Ds, reloaded);
        assert!(
            !cpu.jit_direct
                .segment_layout(block.id())
                .expect("live block layout")
                .data_matches(&cpu),
            "{label}: reloading DS must invalidate a block that uses DS"
        );
    }
}

/// THE MODE13 HALF, and for the control-word pair it is the only place a STATIC-versus-DYNAMIC
/// width disagreement can surface at all.
///
/// The static registration names `word_reads`/`word_stores`; the emitted completion increments
/// the dynamic mode13 counters. `run.rs` then computes `ram_word_reads = word_reads -
/// mode13_word_reads` with a plain, non-saturating subtraction guarded only by a `debug_assert`.
/// If the emitter incremented the DWORD mode13 slot while the block declared a word access, the
/// dword side underflows to a `u64` near 2^64 and is charged straight to the bus. Outside the
/// aperture the dynamic counters never move and the disagreement is invisible.
#[test]
fn control_word_forms_match_the_interpreter_in_ram_and_in_the_aperture() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    for store in [false, true] {
        for target in [RAM, MODE13] {
            let code = control_word_case(store, target);
            let mut memory = vec![0; 0x000b_0000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            memory[target as usize..target as usize + 2].copy_from_slice(&0x0e7fu16.to_le_bytes());
            memory[target as usize + 2..target as usize + 4]
                .copy_from_slice(&0xbeefu16.to_le_bytes());

            let mut native = fresh();
            let mut interp = fresh();
            make_data_segments_flat(&mut native);
            make_data_segments_flat(&mut interp);
            native.registers.eip = ENTRY;
            interp.registers.eip = ENTRY;
            let mut native_bus = TestBus::with_memory(memory.clone());
            let mut interp_bus = TestBus::with_memory(memory);
            for bus in [&mut native_bus, &mut interp_bus] {
                bus.direct_pages_enabled = true;
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.uniform_native_fetches = true;
            }
            let starts = [ENTRY, ENTRY + 6, ENTRY + 12];
            decode_fixture(&mut native, &mut native_bus, &starts);
            decode_fixture(&mut interp, &mut interp_bus, &starts);
            for (cpu, bus) in [
                (&mut native, &mut native_bus),
                (&mut interp, &mut interp_bus),
            ] {
                map_direct_page(
                    cpu,
                    bus,
                    target,
                    target,
                    jit::fast_map::PagePermissions::UNPAGED,
                    true,
                    true,
                );
                cpu.fpu = X87::default();
                cpu.fpu.control = 0x037f;
            }
            let block = install_fixture_block(&mut native, ENTRY);
            for (cpu, bus) in [
                (&mut native, &mut native_bus),
                (&mut interp, &mut interp_bus),
            ] {
                cpu.set_eip(ENTRY);
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.fp_rem = 3;
                cpu.core_clocks_so_far = 0;
                bus.trace = BusTrace::default();
            }
            let label = format!("{} at {target:#x}", if store { "fnstcw" } else { "fldcw" });
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "{label}: did not run directly"
            );
            for _ in 0..block.span().instructions {
                interp.cycle(&mut interp_bus).unwrap();
            }

            assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
            assert_eq!(
                crate::tests::settled_registers(&native),
                crate::tests::settled_registers(&interp),
                "{label}: registers"
            );
            assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
            assert_eq!(
                native.elapsed_clocks, interp.elapsed_clocks,
                "{label}: core clocks"
            );
            assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
            assert_eq!(
                native_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "{label}: bus clocks"
            );
            if store {
                assert_eq!(
                    u16::from_le_bytes(
                        native_bus.memory[target as usize + 2..target as usize + 4]
                            .try_into()
                            .unwrap()
                    ),
                    0xbeef,
                    "{label}: the store stayed two bytes wide"
                );
            } else {
                assert_eq!(native.fpu.control, 0x0e7f, "{label}: loaded control word");
            }
        }
    }
}

/// The 0xDA m32int aperture fixture, modelled directly on
/// `control_word_forms_match_the_interpreter_in_ram_and_in_the_aperture` above: drive-based,
/// `direct_page_clocks` on, comparing the aggregate bus clocks rather than a per-access trace,
/// because native execution batches the whole compiled window and emits no per-access log.
///
/// `esi`-relative addressing (mod=10, rm=110), same shape as the control-word case, so a
/// `read_segment` that wrongly dropped DS from the block's mask would be caught the same way.
fn int_binary_memory_case(extension: u8, target: u32) -> Vec<u8> {
    let mut code = vec![0xdau8, 0x86 | (extension << 3)];
    code.extend_from_slice(&target.to_le_bytes());
    code.push(0x8b);
    code.push(0x96); // mov edx,[esi+disp32]
    code.extend_from_slice(&(target + 4).to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    code
}

#[test]
fn int_binary_memory_matches_the_interpreter_in_ram_and_in_the_aperture() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    const EXTENSION: u8 = 0; // FIADD
    for target in [RAM, MODE13] {
        let code = int_binary_memory_case(EXTENSION, target);
        let mut memory = vec![0; 0x000b_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[target as usize..target as usize + 4].copy_from_slice(&4i32.to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        native.registers.eip = ENTRY;
        interp.registers.eip = ENTRY;
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 6, ENTRY + 12];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                target,
                target,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
            cpu.fpu = X87::default();
            cpu.fpu.push(5.0);
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            cpu.set_eip(ENTRY);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 3;
            cpu.core_clocks_so_far = 0;
            bus.trace = BusTrace::default();
        }
        let label = format!("fiadd at {target:#x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: did not run directly"
        );
        for _ in 0..block.span().instructions {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
        assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{label}: bus clocks"
        );
        assert_eq!(native.fpu.get(0), 9.0, "{label}: FIADD result"); // 5.0 + 4
    }
}

/// Value, flag and timing identity against the interpreter, on both a plain-RAM target and the
/// mode13 aperture. The mode13 half is not just coverage: an under-declared read trips a
/// debug_assert in the run loop there, which is a loud failure, whereas the RAM half only shows up
/// as a bus-clock difference.
#[test]
fn imul_memory_form_matches_the_interpreter_and_its_bus_clocks() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    // Seeds chosen so every failure mode has a discriminating case: a small product that overflows
    // nothing, a product that overflows 32 bits (CF/OF set), a negative times a positive (an
    // unsigned lowering disagrees), and 0x80000000 squared. The overflow cases are also the only
    // ones that separate `imul r32` from the REX.W form, whose 64-bit product always fits.
    for (seed_dst, seed_src) in [
        (3u32, 7u32),
        (0x0001_0000, 0x0001_0000),
        (0xffff_fffe, 0x0000_0003),
        (0x8000_0000, 0x8000_0000),
        (0x7fff_ffff, 0x0000_0002),
        (0x0000_0000, 0xdead_beef),
    ] {
        for target in [RAM, MODE13] {
            let code = imul_mem_case(target);
            let mut memory = vec![0; 0x000b_0000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            memory[target as usize..target as usize + 4].copy_from_slice(&seed_src.to_le_bytes());

            let mut native = fresh();
            let mut interp = fresh();
            make_data_segments_flat(&mut native);
            make_data_segments_flat(&mut interp);
            native.registers.eip = ENTRY;
            interp.registers.eip = ENTRY;
            let mut native_bus = TestBus::with_memory(memory.clone());
            let mut interp_bus = TestBus::with_memory(memory);
            for bus in [&mut native_bus, &mut interp_bus] {
                bus.direct_pages_enabled = true;
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.uniform_native_fetches = true;
            }
            let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
            decode_fixture(&mut native, &mut native_bus, &starts);
            decode_fixture(&mut interp, &mut interp_bus, &starts);
            for (cpu, bus) in [
                (&mut native, &mut native_bus),
                (&mut interp, &mut interp_bus),
            ] {
                map_direct_page(
                    cpu,
                    bus,
                    target,
                    target,
                    jit::fast_map::PagePermissions::UNPAGED,
                    true,
                    false,
                );
            }
            let block = install_fixture_block(&mut native, ENTRY);

            for cpu in [&mut native, &mut interp] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                cpu.registers.gpr.fill(0xdead_beef);
                cpu.registers.set_esp(0xc000);
                // ESI is the address base and must be zero for the effective address to be
                // `target`. Set AFTER the fill, or the load reads a wild address.
                cpu.registers.set_esi(0);
                cpu.registers.set_ebx(seed_dst);
                // AF and the reserved bit are seeded SET so a tail that publishes a stale or
                // over-wide flag word is visible: IMUL must leave SF, ZF, AF and PF alone.
                cpu.registers.eflags = 0x0296;
                cpu.pending_flags = PendingFlags::default();
                cpu.registers.eip = ENTRY;
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.core_clocks_so_far = 0;
            }
            native_bus.trace = BusTrace::default();
            interp_bus.trace = BusTrace::default();

            let label = format!("dst={seed_dst:#010x} src={seed_src:#010x} target={target:#x}");
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "{label}: must run natively"
            );
            for _ in 0..3 {
                interp.cycle(&mut interp_bus).unwrap();
            }

            assert_eq!(
                crate::tests::settled_registers(&native),
                crate::tests::settled_registers(&interp),
                "{label}: registers"
            );
            assert_eq!(native.eflags(), interp.eflags(), "{label}: eflags");
            assert_eq!(
                native.elapsed_clocks, interp.elapsed_clocks,
                "{label}: core clocks"
            );
            assert_eq!(
                native_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "{label}: BUS clocks, the registration check"
            );

            // Pin the concrete product and the concrete CF/OF, not only agreement: if both sides
            // were wrong in the same direction the comparisons above would still pass.
            let product = i64::from(seed_dst as i32) * i64::from(seed_src as i32);
            assert_eq!(
                native.registers.ebx(),
                product as u32,
                "{label}: truncated product"
            );
            let significant = product != i64::from(product as u32 as i32);
            assert_eq!(
                native.eflags() & (crate::FLAG_CF | crate::FLAG_OF) != 0,
                significant,
                "{label}: CF and OF track the 32-bit truncation, not the 64-bit product"
            );
        }
    }
}

/// The destination register is also the address base. The source must be read BEFORE the
/// destination is written, which is what the interpreter does (`read_operand_sized` then
/// `write_gpr_sized`) and what reading the pointer before the multiply gives here.
#[test]
fn imul_memory_form_handles_the_destination_as_its_own_address_base() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    // 0F AF /r, mod=10 rm=110 (esi), reg=110 (esi) -> modrm 0b10_110_110 = 0xb6. `imul esi,
    // [esi+0]`, with ESI seeded to TARGET so the address is TARGET and the destination is ESI.
    let mut code = vec![0x0fu8, 0xaf, 0xb6, 0x00, 0x00, 0x00, 0x00];
    code.extend_from_slice(&[0x89, 0xff, 0x89, 0xff, 0xf4]);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&7u32.to_le_bytes());

    let mut native = fresh();
    let mut interp = fresh();
    make_data_segments_flat(&mut native);
    make_data_segments_flat(&mut interp);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        bus.uniform_native_fetches = true;
    }
    let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            TARGET,
            TARGET,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
    }
    let block = install_fixture_block(&mut native, ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0xc000);
        cpu.registers.set_esi(TARGET);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp),
        "aliased dst/base"
    );
    assert_eq!(
        native.registers.esi(),
        TARGET.wrapping_mul(7),
        "the source must be read before the destination is written"
    );
}

/// The ONLY fixture that exercises the materialize-then-write path. Slot 0 is an ADD, which leaves
/// a live pending descriptor; the IMUL that follows must materialize it (including any CF
/// override) and then write only CF and OF, leaving SF, ZF, AF and PF as the ADD computed them.
/// Without this fixture the whole RBP-shadow argument runs with no descriptor live and is
/// untested, and `emit_clear_pending` has no catcher at all.
#[test]
fn imul_memory_form_materializes_a_live_descriptor_first() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    for (seed_dst, seed_add, seed_src) in [
        (0x0000_0005u32, 0x0000_0003u32, 0x0000_0007u32),
        (0xffff_ffffu32, 0x0000_0001u32, 0x0001_0000u32),
        (0x8000_0000u32, 0x8000_0000u32, 0x0000_0003u32),
    ] {
        // 01 cb = add ebx, ecx, then the IMUL, then one register move.
        let mut code = vec![0x01u8, 0xcb];
        code.extend_from_slice(&imul_mem_case(TARGET)[..7]);
        code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&seed_src.to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 2, ENTRY + 9];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                TARGET,
                TARGET,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                false,
            );
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for cpu in [&mut native, &mut interp] {
            cpu.halted = false;
            cpu.interrupt_shadow = false;
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esp(0xc000);
            cpu.registers.set_esi(0);
            cpu.registers.set_ebx(seed_dst);
            cpu.registers.set_ecx(seed_add);
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.registers.eip = ENTRY;
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let label = format!("dst={seed_dst:#010x} add={seed_add:#010x} src={seed_src:#010x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: must run natively"
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        // The ARCHITECTURAL flags. A tail that skipped `emit_clear_pending` and left a stale
        // descriptor owning the six arithmetic bits diverges here; a tail that materialized
        // eagerly and agrees on every architectural bit does not, and is not meant to.
        assert_eq!(native.eflags(), interp.eflags(), "{label}: eflags");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
    }
}

#[test]
fn imul_word_operand_memory_form_remains_interpreter_only() {
    // 66 0F AF 9E <disp32>: IMUL BX, [esi+disp32]. The OperandSize::Word gate is the only thing
    // stopping this, and `write_gpr_sized` at Word MERGES into the low 16 bits instead of
    // replacing all 32, so lowering it as the 32-bit form would clobber the destination's high
    // half and compute CF/OF against the wrong width.
    //
    // PAIRED with `imul_memory_form_is_lowered` below. On its own this assertion passes for any
    // reason the harness stops compiling, including the classify arm being unreachable.
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let mut code = vec![0x66u8];
    code.extend_from_slice(&imul_mem_case(TARGET));
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    // Three warmed starts. Warming only the entry makes slot 1 miss, the walk stops at Retry, and
    // the fewer-than-three-slots gate returns the same None a real reject would.
    let starts = [ENTRY, ENTRY + 8, ENTRY + 10];
    decode_fixture(&mut cpu, &mut bus, &starts);
    // Map the target page, or the compile refuses for want of a direct-page mapping and the
    // assertion below passes for a reason unrelated to the opcode.
    map_direct_page(
        &mut cpu,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    assert!(
        jit::direct::compile(&mut cpu, ENTRY, true).is_none(),
        "66-prefixed memory IMUL must stay interpreter-only"
    );
}

#[test]
fn imul_memory_form_is_lowered() {
    // The positive half of the guard above, and the ONLY test that can detect the classify arm
    // being keyed on the low opcode byte, where the `u8::try_from(insn.opcode)` truncation makes it
    // unreachable for a two-byte opcode. Every negative assertion passes when that happens.
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let code = imul_mem_case(TARGET);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    let instructions = outcome
        .is_some()
        .then(|| outcome.unwrap().span.instructions);
    assert_eq!(
        instructions,
        Some(3),
        "the memory IMUL must admit and carry the whole three-slot block"
    );
}

/// IMUL r/m32, one-operand signed, memory form: `imul dword [esi + target]`.
///
/// F7 /5, mod=10 (disp32) rm=110 (esi), reg=101 (the /5 sub-opcode) -> modrm 0b10_101_110 = 0xae.
/// ESI is held at zero by every fixture so the effective address is `target`. Slots 1 and 2 are
/// register moves that touch neither flags nor ESI, so the block carries exactly one memory access.
fn grp3_imul_mem_case(target: u32) -> Vec<u8> {
    let mut code = vec![0xf7u8, 0xae];
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
    code
}

/// THE REGISTRATION TEST, and note the raw-clocks assertion runs the OPPOSITE way from the 0x0FAF
/// one above. The whole group-3 arm returns clocks(2) for every sub-opcode, which is already the
/// DirectKind default, so this form must NOT carry a raw_clocks field. Asserting 2 + 2 + 2 is what
/// catches a well-meaning edit that adds one by analogy with ImulMem's 9.
#[test]
fn grp3_imul_memory_form_declares_its_read_and_its_segment() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let code = grp3_imul_mem_case(TARGET);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    let starts = [ENTRY, ENTRY + 6, ENTRY + 8];
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let block = install_fixture_block(&mut cpu, ENTRY);

    assert_eq!(block.span().instructions, 3, "whole block admitted");
    assert_eq!(block.byte_reads(), 0, "no byte read");
    assert_eq!(block.word_reads(), 0, "no word read");
    assert_eq!(block.dword_reads(), 1, "dword-read declaration");
    assert!(block.has_wide_accesses(), "wide-access declaration");
    // Group 3 returns clocks(2) for every sub-opcode and both operand forms, so all three slots
    // charge 2. A raw_clocks arm added here by analogy with ImulMem's 9 shows up as 9 + 2 + 2.
    assert_eq!(block.raw_clocks(), 2 + 2 + 2, "charged core clocks");

    assert!(
        cpu.jit_direct
            .segment_layout(block.id())
            .expect("live block layout")
            .data_matches(&cpu),
        "matches at compile time"
    );
    let mut reloaded = cpu.registers.segment(SegmentIndex::Ds);
    reloaded.base = 0x1_0000;
    cpu.registers.set_segment(SegmentIndex::Ds, reloaded);
    assert!(
        !cpu.jit_direct
            .segment_layout(block.id())
            .expect("live block layout")
            .data_matches(&cpu),
        "reloading DS must invalidate a block that reads through DS"
    );
}

#[test]
fn grp3_imul_memory_form_matches_the_interpreter_and_its_bus_clocks() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    // The first pair is THE discriminating one and the reason this slice needs its own encoder
    // primitive. 0xFFFFFFFF * 2 signed is -2, so EDX:EAX = 0xFFFFFFFF_FFFFFFFE and the product DOES
    // sign-extend from the low half, leaving CF and OF CLEAR. Unsigned it is 0x00000001_FFFFFFFE
    // with the high half nonzero, so CF and OF are SET. A /4 encoding differs in both EDX and the
    // flags on that pair and agrees with /5 on every small positive one.
    for (seed_eax, seed_src) in [
        (0xffff_ffffu32, 0x0000_0002u32),
        (0x0000_0003, 0x0000_0007),
        (0x0001_0000, 0x0001_0000),
        (0x8000_0000, 0x8000_0000),
        (0xffff_ffff, 0xffff_ffff),
        (0x7fff_ffff, 0x0000_0002),
    ] {
        for target in [RAM, MODE13] {
            let code = grp3_imul_mem_case(target);
            let mut memory = vec![0; 0x000b_0000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            memory[target as usize..target as usize + 4].copy_from_slice(&seed_src.to_le_bytes());

            let mut native = fresh();
            let mut interp = fresh();
            make_data_segments_flat(&mut native);
            make_data_segments_flat(&mut interp);
            let mut native_bus = TestBus::with_memory(memory.clone());
            let mut interp_bus = TestBus::with_memory(memory);
            for bus in [&mut native_bus, &mut interp_bus] {
                bus.direct_pages_enabled = true;
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.uniform_native_fetches = true;
            }
            let starts = [ENTRY, ENTRY + 6, ENTRY + 8];
            decode_fixture(&mut native, &mut native_bus, &starts);
            decode_fixture(&mut interp, &mut interp_bus, &starts);
            for (cpu, bus) in [
                (&mut native, &mut native_bus),
                (&mut interp, &mut interp_bus),
            ] {
                map_direct_page(
                    cpu,
                    bus,
                    target,
                    target,
                    jit::fast_map::PagePermissions::UNPAGED,
                    true,
                    false,
                );
            }
            let block = install_fixture_block(&mut native, ENTRY);

            for cpu in [&mut native, &mut interp] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                cpu.registers.gpr.fill(0xdead_beef);
                cpu.registers.set_esp(0xc000);
                cpu.registers.set_esi(0);
                cpu.registers.set_eax(seed_eax);
                // EDX seeded to a recognisable non-zero value: the instruction must REPLACE it with
                // the product's high half, not merge into it.
                cpu.registers.set_edx(0x1234_5678);
                // AF and the reserved bit seeded SET. One-operand IMUL writes only CF and OF.
                cpu.registers.eflags = 0x0296;
                cpu.pending_flags = PendingFlags::default();
                cpu.registers.eip = ENTRY;
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.core_clocks_so_far = 0;
            }
            native_bus.trace = BusTrace::default();
            interp_bus.trace = BusTrace::default();

            let label = format!("eax={seed_eax:#010x} src={seed_src:#010x} target={target:#x}");
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "{label}: must run natively"
            );
            for _ in 0..3 {
                interp.cycle(&mut interp_bus).unwrap();
            }

            assert_eq!(
                crate::tests::settled_registers(&native),
                crate::tests::settled_registers(&interp),
                "{label}: registers"
            );
            assert_eq!(native.eflags(), interp.eflags(), "{label}: eflags");
            assert_eq!(
                native.elapsed_clocks, interp.elapsed_clocks,
                "{label}: core clocks"
            );
            assert_eq!(
                native_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "{label}: BUS clocks, the registration check"
            );

            // Pin the concrete SIGNED product and the concrete CF and OF, so an unsigned lowering
            // fails here even if both sides agreed with each other.
            let product = i64::from(seed_eax as i32) * i64::from(seed_src as i32);
            assert_eq!(native.registers.eax(), product as u32, "{label}: low half");
            assert_eq!(
                native.registers.edx(),
                (product >> 32) as u32,
                "{label}: high half"
            );
            let significant = product != i64::from(product as u32 as i32);
            assert_eq!(
                native.eflags() & (crate::FLAG_CF | crate::FLAG_OF) != 0,
                significant,
                "{label}: CF and OF use the SIGNED rule, not the unsigned one"
            );
        }
    }
}

/// The address is built from EAX and EDX, the two registers the instruction implicitly overwrites,
/// and through the scaled-index path rather than a bare base. The read must complete before either
/// home is written, which is an ordering requirement on `emit_ram_read_pointer` running first and
/// not an absence of aliasing: this form has a LARGER aliasing surface than the register form.
#[test]
fn grp3_imul_memory_form_handles_an_address_built_from_its_own_destinations() {
    const ENTRY: u32 = 0x101;
    const BASE: u32 = 0x0003_0000;
    // F7 /5 with mod=10 rm=100 (SIB) -> modrm 0b10_101_100 = 0xac. SIB scale=2 (x4), index=edx(2),
    // base=eax(0) -> 0b10_010_000 = 0x90. So `imul dword [eax + edx*4 + disp32]`.
    let mut code = vec![0xf7u8, 0xac, 0x90];
    code.extend_from_slice(&0u32.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0x89, 0xff, 0xf4]);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // EAX = BASE, EDX = 4, so the address is BASE + 16.
    memory[BASE as usize + 16..BASE as usize + 20].copy_from_slice(&7u32.to_le_bytes());

    let mut native = fresh();
    let mut interp = fresh();
    make_data_segments_flat(&mut native);
    make_data_segments_flat(&mut interp);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.report_batch_clocks = true;
        bus.uniform_native_fetches = true;
    }
    let starts = [ENTRY, ENTRY + 7, ENTRY + 9];
    decode_fixture(&mut native, &mut native_bus, &starts);
    decode_fixture(&mut interp, &mut interp_bus, &starts);
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            BASE,
            BASE,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
    }
    let block = install_fixture_block(&mut native, ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr.fill(0);
        cpu.registers.set_esp(0xc000);
        cpu.registers.set_eax(BASE);
        cpu.registers.set_edx(4);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.registers.eip = ENTRY;
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp),
        "aliased base and index"
    );
    assert_eq!(
        native.registers.eax(),
        BASE.wrapping_mul(7),
        "the address must be resolved before either destination home is written"
    );
}

/// The only fixture that exercises materialize-then-write, and the only catcher for
/// `emit_clear_pending`. Slot 0 is an ADD, which leaves a live pending descriptor.
#[test]
fn grp3_imul_memory_form_materializes_a_live_descriptor_first() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    for (seed_eax, seed_add, seed_src) in [
        (0x0000_0005u32, 0x0000_0003u32, 0x0000_0007u32),
        (0xffff_fffdu32, 0x0000_0002u32, 0x0000_0002u32),
        (0x8000_0000u32, 0x8000_0000u32, 0x0000_0003u32),
    ] {
        // 01 c8 = add eax, ecx, then the IMUL, then one register move.
        let mut code = vec![0x01u8, 0xc8];
        code.extend_from_slice(&grp3_imul_mem_case(TARGET)[..6]);
        code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[TARGET as usize..TARGET as usize + 4].copy_from_slice(&seed_src.to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 2, ENTRY + 8];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                TARGET,
                TARGET,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                false,
            );
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for cpu in [&mut native, &mut interp] {
            cpu.halted = false;
            cpu.interrupt_shadow = false;
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esp(0xc000);
            cpu.registers.set_esi(0);
            cpu.registers.set_eax(seed_eax);
            cpu.registers.set_ecx(seed_add);
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.registers.eip = ENTRY;
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let label = format!("eax={seed_eax:#010x} add={seed_add:#010x} src={seed_src:#010x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: must run natively"
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native.eflags(), interp.eflags(), "{label}: eflags");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
    }
}

/// The negative list. Each case is PAIRED with `grp3_imul_memory_form_is_lowered` below; on its own
/// any of these passes whenever the harness stops compiling for a reason unrelated to the opcode.
/// Every fixture maps the target page, because not mapping it is exactly how the previous IMUL
/// guard rail in this repository passed vacuously for two slices.
#[test]
fn grp3_imul_neighbouring_forms_remain_interpreter_only() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let mut mul_mem = vec![0xf7u8, 0xa6]; // F7 /4 mod=10 rm=110: MUL dword [esi+disp32], UNSIGNED
    mul_mem.extend_from_slice(&TARGET.to_le_bytes());
    let mut byte_imul = vec![0xf6u8, 0xae]; // F6 /5 mod=10 rm=110: IMUL byte [esi+disp32]
    byte_imul.extend_from_slice(&TARGET.to_le_bytes());
    // The F7 /5 REGISTER form used to be a fourth case here. The rejected-row campaign's Slice 2
    // lowered it (`DirectKind::ImulRegAcc`), so its admission is now pinned positively in
    // `group3_dword_neg_register_form_is_lowered` and its behaviour in `cpu_jit_f7_group_test.rs`.
    //
    // The 66-prefixed WORD memory IMUL used to be a fifth. The S3 policy widening routes every
    // Word group-3 form to an `InterpretOne` call-out BEFORE the /5 arm is reached, so it no
    // longer stays out of the block -- but it still never reaches that arm, which is what this
    // list is about. The claim moved to `group3_word_subops_join_as_call_outs_not_lowerings`
    // (cpu_jit_test_imm_test.rs), which asserts the slot class directly rather than inferring it
    // from the block ending.
    //
    // The two cases left are the ones that must NOT reach either /5 arm and have no call-out to
    // take instead.
    for (code, why) in [
        (
            mul_mem,
            "MUL /4 memory: reaching the /5 arm would emit a SIGNED multiply",
        ),
        (
            byte_imul,
            "F6 /5 byte IMUL: reaching the /5 arm would read a dword and write EAX and EDX",
        ),
    ] {
        let mut memory = vec![0; 0x0004_0000];
        let mut block = code.clone();
        block.extend_from_slice(&[0x89, 0xf6, 0x89, 0xff, 0xf4]);
        memory[ENTRY as usize..ENTRY as usize + block.len()].copy_from_slice(&block);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        // Three warmed starts. Warming only the entry makes slot 1 miss, the walk stops at Retry,
        // and the fewer-than-three-slots gate returns the same None a real reject would.
        let starts = [
            ENTRY,
            ENTRY + code.len() as u32,
            ENTRY + code.len() as u32 + 2,
        ];
        decode_fixture(&mut cpu, &mut bus, &starts);
        map_direct_page(
            &mut cpu,
            &mut bus,
            TARGET,
            TARGET,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            false,
        );
        assert!(
            jit::direct::compile(&mut cpu, ENTRY, true).is_none(),
            "{code:02x?} must stay interpreter-only: {why}"
        );
    }
}

#[test]
fn grp3_imul_memory_form_is_lowered() {
    // The positive half of the guard above. Without it every assertion there passes whenever the
    // classify arm is unreachable or the fixture cannot compile anything at all.
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    let code = grp3_imul_mem_case(TARGET);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let starts = [ENTRY, ENTRY + 6, ENTRY + 8];
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        TARGET,
        TARGET,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
    let instructions = outcome
        .is_some()
        .then(|| outcome.unwrap().span.instructions);
    assert_eq!(
        instructions,
        Some(3),
        "the group-3 memory IMUL must admit and carry the whole three-slot block"
    );
}

struct DirectTimingCase {
    name: &'static str,
    opcode: &'static [u8],
    expected_raw_clocks: u32,
    terminal: bool,
    eflags: u32,
}

fn direct_timing_cases() -> Vec<DirectTimingCase> {
    vec![
        DirectTimingCase {
            name: "mov register",
            opcode: &[0x89, 0xc8],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov byte register",
            opcode: &[0x88, 0xcc],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov immediate",
            opcode: &[0xb8, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "mov byte immediate",
            opcode: &[0xb4, 0x7f],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "lea",
            opcode: &[0x8d, 0x44, 0x8b, 0x10],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "inc register",
            opcode: &[0x40],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "alu register",
            opcode: &[0x01, 0xc8],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu immediate",
            opcode: &[0x83, 0xc0, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu byte immediate",
            opcode: &[0x80, 0xc4, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu memory source",
            opcode: &[0x03, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu dword memory destination",
            opcode: &[0x01, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "alu byte memory destination",
            opcode: &[0x80, 0x05, 0x00, 0x30, 0x00, 0x00, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "compare memory destination",
            opcode: &[0x83, 0x3d, 0x00, 0x30, 0x00, 0x00, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "test register",
            opcode: &[0x85, 0xc0],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "shift register",
            opcode: &[0xc1, 0xe8, 0x01],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "load byte",
            opcode: &[0x8a, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "load dword",
            opcode: &[0x8b, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store byte",
            opcode: &[0x88, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store dword",
            opcode: &[0x89, 0x0d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs load byte",
            opcode: &[0xa0, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs load dword",
            opcode: &[0xa1, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs store byte",
            opcode: &[0xa2, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "moffs store dword",
            opcode: &[0xa3, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store byte immediate",
            opcode: &[0xc6, 0x05, 0x00, 0x30, 0x00, 0x00, 0x7f],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "store dword immediate",
            opcode: &[0xc7, 0x05, 0x00, 0x30, 0x00, 0x00, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "memory inc",
            opcode: &[0xff, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x203,
        },
        DirectTimingCase {
            name: "push register",
            opcode: &[0x50],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "push immediate",
            opcode: &[0x68, 0x78, 0x56, 0x34, 0x12],
            expected_raw_clocks: 6,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "pop register",
            opcode: &[0x58],
            expected_raw_clocks: 8,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "call relative",
            opcode: &[0xe8, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "jump near",
            opcode: &[0xe9, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "jump short",
            opcode: &[0xeb, 0x20],
            expected_raw_clocks: 7,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "return",
            opcode: &[0xc3],
            expected_raw_clocks: 10,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "return and release",
            opcode: &[0xc2, 0x08, 0x00],
            expected_raw_clocks: 10,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "short jcc fallthrough",
            opcode: &[0x74, 0x20],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "short jcc taken",
            opcode: &[0x74, 0x20],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x242,
        },
        DirectTimingCase {
            name: "near jcc fallthrough",
            opcode: &[0x0f, 0x85, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x242,
        },
        DirectTimingCase {
            name: "near jcc taken",
            opcode: &[0x0f, 0x85, 0x20, 0x00, 0x00, 0x00],
            expected_raw_clocks: 3,
            terminal: true,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 register",
            opcode: &[0xd8, 0xc0],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 memory load",
            opcode: &[0xd9, 0x05, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 memory store and pop",
            opcode: &[0xd9, 0x1d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        // The control-word pair. `expected_raw_clocks` is 4 for both because it is 2 + 2 from the
        // two filler moves and ZERO from the x87 slot: `DirectKind::raw_clocks` returns 0 for
        // `Self::X87` and the whole x87 cost goes through `weighted_fp_clocks` instead. So this
        // number is the catcher for an ADDED raw_clocks arm (it would read 8), while the
        // 4-versus-14 split between these two instructions is caught by `elapsed_clocks` below.
        // 0xDC mod=3, ST(1) op ST(0). raw_clocks 4 is the two filler moves; the x87 slot
        // contributes zero to `DirectKind::raw_clocks` and twenty to `weighted_fp_clocks`, so
        // this number catches an ADDED raw_clocks arm and `elapsed_clocks` catches a wrong
        // twenty. The register form declares no memory, so raw_bus_clocks moving at all would
        // mean the registration leaked a memory property.
        // rm = 0, so the destination is ST(0) and both operands are the one register this
        // harness populates. rm = 1 would address an EMPTY ST(1), and `emit_load_physical`'s tag
        // guard would side-exit at slot 0, leaving the native side retiring nothing and the
        // comparison meaningless. It also exercises the destination-equals-source case.
        DirectTimingCase {
            name: "x87 sti register binary",
            opcode: &[0xdc, 0xc8],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 fldcw m16",
            opcode: &[0xd9, 0x2d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "x87 fnstcw m16",
            opcode: &[0xd9, 0x3d, 0x00, 0x30, 0x00, 0x00],
            expected_raw_clocks: 4,
            terminal: false,
            eflags: 0x202,
        },
        // Slice 7. These four are EXACT pins and that is the point of putting them here rather
        // than leaving the charge to the slice's own accumulation ladder. A ladder that asserts
        // the SCALED clock is one-sided: at four slots `floor((14n + 4) / 12)` catches an
        // undercharge of 9 or of the `_ => 2` default, and misses an overcharge of 15 or 16,
        // because those still floor to the same value. An exact `raw_clocks()` compare is
        // two-sided by construction, and the review that caught the asymmetry is why both now
        // exist. The standing rule this amends: an accumulation fixture is NECESSARY for a
        // block-scaled charge and NOT SUFFICIENT -- it must be paired with an exact pin.
        //
        // `imul ebx, eax, imm` -- modrm 0b11_011_000 = 0xd8, reg = EBX is the destination and
        // rm = EAX the source. clocks(14) for the three-operand form against the two-operand
        // form's clocks(9) and the table default's 2, plus 2 each for the two register moves.
        DirectTimingCase {
            name: "three-operand imul register imm32",
            opcode: &[0x69, 0xd8, 0x03, 0x00, 0x00, 0x00],
            expected_raw_clocks: 14 + 2 + 2,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "three-operand imul register imm8",
            opcode: &[0x6b, 0xd8, 0x03],
            expected_raw_clocks: 14 + 2 + 2,
            terminal: false,
            eflags: 0x202,
        },
        // The byte-lane register ALU, both operand orders. `execute_alu_decoded` returns one
        // `Ok(clocks(2))` for all six ALU forms, so these ride the `_ => 2` default CORRECTLY --
        // and the pin is what says so. Without it, "the default is right here" is an argument
        // rather than a measurement, and an arm added later for the wrong reason would pass.
        // `cmp cl, al` (form 0: rm is the destination) and `cmp al, cl` (form 2: reg is).
        DirectTimingCase {
            name: "byte alu register destination",
            opcode: &[0x38, 0xc1],
            expected_raw_clocks: 2 + 2 + 2,
            terminal: false,
            eflags: 0x202,
        },
        DirectTimingCase {
            name: "byte alu register source",
            opcode: &[0x3a, 0xc1],
            expected_raw_clocks: 2 + 2 + 2,
            terminal: false,
            eflags: 0x202,
        },
    ]
}

fn run_direct_timing_case(mode: GswMode, uniform_fetches: bool, case: &DirectTimingCase) {
    const ENTRY: u32 = 0x101;
    const DATA: usize = 0x3000;
    const STACK: usize = 0x5000;

    let mut code = case.opcode.to_vec();
    let mut starts = vec![ENTRY];
    if !case.terminal {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&[0x89, 0xf6]);
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&[0x89, 0xff]);
    }
    starts.push(ENTRY + code.len() as u32);
    // The block-ender, never executed: the interpreted role runs exactly `span().instructions`
    // cycles and the block stops here. HLT, and not the `66 87 c0` (XCHG AX,AX) this used to be.
    // That one was chosen as an opcode the classifier refused, and the S3 policy widening admitted
    // the whole XCHG family as `InterpretOne` call-outs, which grew every case's block from three
    // slots to four and put a call-out inside a matrix that compares `pending_flags` between the
    // two roles (the helper publishes a settled word where the interpreter leaves a descriptor).
    // HLT cannot be admitted by any future slice -- it stops the machine -- so the fixture's
    // boundary is a property of the instruction rather than of today's allowlist.
    code.push(0xf4);
    let mut pristine = vec![0; 0x7000];
    pristine[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    pristine[DATA..DATA + 4].copy_from_slice(&2.5f32.to_bits().to_le_bytes());
    pristine[STACK..STACK + 4].copy_from_slice(&0x180u32.to_le_bytes());

    let mut direct = flat_stack_cpu(ENTRY);
    let mut interpreter = flat_stack_cpu(ENTRY);
    direct.set_mode(mode);
    interpreter.set_mode(mode);
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    let mut interpreter_bus = TestBus::with_memory(pristine);
    for bus in [&mut direct_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        bus.uniform_native_fetches = uniform_fetches;
    }
    // Declare the emission arm beside the bus that implies it. `install_fixture_block` calls
    // `direct::compile` straight through, so it never reaches the `try_direct_continuation`
    // synchronisation that production uses, and without this line the field would sit at its
    // `true` seed for every case — leaving this whole interpreter-versus-native matrix testing
    // only the WITH-preamble shape, on both arms of the `uniform_fetches` loop (review
    // finding). With it, the `uniform_fetches == true` half of the matrix now checks the
    // trace-elided emission lane by lane against the interpreter, which is the coverage the
    // elision needs and the only place it can come from.
    direct.jit_direct.native_fetch_trace = !uniform_fetches;
    interpreter.jit_direct.native_fetch_trace = !uniform_fetches;
    decode_fixture(&mut direct, &mut direct_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    for (cpu, bus) in [
        (&mut direct, &mut direct_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        for page in [0x3000, 0x4000, 0x5000] {
            map_direct_page(
                cpu,
                bus,
                page,
                page,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
        }
    }
    if case
        .opcode
        .first()
        .is_some_and(|opcode| (0xd8..=0xdf).contains(opcode))
    {
        direct.fpu.push(1.25);
    }
    let block = install_fixture_block(&mut direct, ENTRY);
    assert_eq!(
        block.raw_clocks(),
        case.expected_raw_clocks,
        "{} {mode:?} raw core table",
        case.name
    );
    assert_eq!(
        block.span().instructions,
        if case.terminal { 1 } else { 3 },
        "{} {mode:?} block shape",
        case.name
    );

    for cpu in [&mut direct, &mut interpreter] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = [
            0x1122_3344,
            3,
            0x5566_7788,
            0x3000,
            STACK as u32,
            0,
            0x40,
            0x80,
        ];
        cpu.registers.eflags = case.eflags;
        cpu.pending_flags = PendingFlags::default();
        cpu.fpu = X87::default();
        cpu.fpu.push(1.25);
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.fp_rem = 3;
        cpu.core_clocks_so_far = 0;
    }
    direct_bus.trace = BusTrace::default();
    interpreter_bus.trace = BusTrace::default();

    assert!(
        direct
            .try_run_direct_block_for_test(&mut direct_bus, block)
            .unwrap(),
        "{} {mode:?} did not run directly",
        case.name
    );
    for _ in 0..block.span().instructions {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        direct.elapsed_clocks, interpreter.elapsed_clocks,
        "{} {mode:?} scaled core clocks",
        case.name
    );
    assert_eq!(
        direct.timing_rem, interpreter.timing_rem,
        "{} {mode:?} core remainder",
        case.name
    );
    assert_eq!(
        direct.fp_rem, interpreter.fp_rem,
        "{} {mode:?} x87 remainder",
        case.name
    );
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{} {mode:?} bus clocks",
        case.name
    );
    assert_eq!(
        direct
            .elapsed_clocks
            .saturating_add(direct_bus.trace.elapsed_clocks()),
        interpreter
            .elapsed_clocks
            .saturating_add(interpreter_bus.trace.elapsed_clocks()),
        "{} {mode:?} combined clocks",
        case.name
    );
    assert_eq!(
        crate::tests::settled_registers(&direct),
        crate::tests::settled_registers(&interpreter),
        "{} {mode:?}",
        case.name
    );
    assert_eq!(
        direct.eflags(),
        interpreter.eflags(),
        "{} {mode:?} EFLAGS",
        case.name
    );
    assert_eq!(
        direct.fpu, interpreter.fpu,
        "{} {mode:?} x87 state",
        case.name
    );
    assert_eq!(
        direct_bus.memory, interpreter_bus.memory,
        "{} {mode:?} memory",
        case.name
    );
}

#[test]
fn direct_family_core_and_bus_timing_matches_interpreter_in_486_and_586_modes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for uniform_fetches in [false, true] {
            for case in direct_timing_cases() {
                run_direct_timing_case(mode, uniform_fetches, &case);
            }
        }
    }
}

const QUAKE_SEGMENT_BASE: u32 = 0x1000_0000;
const QUAKE_CS_LIMIT: u32 = 0x016e_ffff;

fn quake_segment_cpu(entry: u32, paging: bool) -> CpuGsw {
    let mut cpu = flat_stack_cpu(entry);
    cpu.set_fast_map_enabled_for_test(true);
    if paging {
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
    }
    cpu.cpl = 3;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x00a7,
            base: QUAKE_SEGMENT_BASE,
            limit: QUAKE_CS_LIMIT,
            access: 0xfb,
            default_size_32: true,
        },
    );
    for segment in [SegmentIndex::Ds, SegmentIndex::Ss, SegmentIndex::Es] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0x00af,
                base: QUAKE_SEGMENT_BASE,
                limit: u32::MAX,
                access: 0xf3,
                default_size_32: true,
            },
        );
    }
    for segment in [SegmentIndex::Fs, SegmentIndex::Gs] {
        cpu.registers.set_segment(
            segment,
            SegmentRegister {
                selector: 0x00cf,
                base: 0,
                limit: 0x00ff_ffff,
                access: 0xf3,
                default_size_32: true,
            },
        );
    }
    cpu.set_eip(entry);
    cpu
}

fn decode_segmented_fixture(cpu: &mut CpuGsw, bus: &mut TestBus, offsets: &[u32]) {
    let cs_base = cpu.registers.cs().base;
    for &offset in offsets {
        cpu.set_eip(offset);
        cpu.fetch_decoded(bus, cs_base.wrapping_add(offset))
            .unwrap();
    }
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn high_segment_page_tables(memory: &mut [u8]) {
    memory[0x3100..0x3104].copy_from_slice(&0x0000_4067u32.to_le_bytes());
    memory[0x4000..0x4004].copy_from_slice(&0x0000_8067u32.to_le_bytes());
}

#[test]
fn quake_descriptors_admit_a_finite_cs_register_loop_natively() {
    const ENTRY: u32 = 0x101;
    let mut memory = vec![0; 0xc000];
    high_segment_page_tables(&mut memory);
    memory[0x8101..0x8109].copy_from_slice(&[
        0x83, 0xc0, 0x01, // add eax,1
        0x83, 0xe9, 0x01, // sub ecx,1
        0x75, 0xf8, // jnz ENTRY
    ]);
    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 3, ENTRY + 6];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.registers.set_eax(5);
        cpu.registers.set_ecx(4);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }
    let entries = native.perf_counters().jit_direct_entries;
    let retired = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..12 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(native.perf_counters().jit_direct_entries - entries, 1);
    assert_eq!(native.perf_counters().jit_direct_insns - retired, 12);
}

#[test]
fn paged_quake_ds_ss_bases_match_load_store_and_call() {
    const ENTRY: u32 = 0x201;
    const TARGET: u32 = 0x240;
    let mut memory = vec![0; 0xc000];
    high_segment_page_tables(&mut memory);
    memory[0x4004..0x4008].copy_from_slice(&0x0000_6067u32.to_le_bytes());
    memory[0x4008..0x400c].copy_from_slice(&0x0000_7067u32.to_le_bytes());
    memory[0x8201..0x8210].copy_from_slice(&[
        0xa1, 0x00, 0x10, 0x00, 0x00, // mov eax,[0x1000]
        0xa3, 0x04, 0x10, 0x00, 0x00, // mov [0x1004],eax
        0xe8, 0x30, 0x00, 0x00, 0x00, // call TARGET
    ]);
    memory[0x6000..0x6004].copy_from_slice(&0x7654_3210u32.to_le_bytes());
    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, ENTRY + 10];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    let permissions = jit::fast_map::PagePermissions {
        writable: true,
        user: true,
    };
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + 0x1000,
        0x6000,
        permissions,
        true,
        true,
    );
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + 0x2000,
        0x7000,
        permissions,
        false,
        true,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    for cpu in [&mut native, &mut interp] {
        cpu.registers.set_esp(0x2004);
        cpu.registers.eflags = 0x246;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }

    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.eflags(), interp.eflags());
    assert_eq!(native.registers.eip, TARGET);
    assert_eq!(native.registers.esp(), 0x2000);
    assert_eq!(
        &native_bus.memory[0x6000..0x6008],
        &interp_bus.memory[0x6000..0x6008]
    );
    assert_eq!(
        &native_bus.memory[0x7000..0x7004],
        &interp_bus.memory[0x7000..0x7004]
    );
    assert_eq!(
        u32::from_le_bytes(native_bus.memory[0x7000..0x7004].try_into().unwrap()),
        0x210
    );
}

#[test]
fn finite_cs_near_returns_run_directly_and_match_interpreter() {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const TARGET: u32 = 0x380;
    const INITIAL_ESP: u32 = 0x2000;

    for (return_bytes, release) in [(&[0xc3][..], 0u32), (&[0xc2, 0x08, 0x00][..], 8)] {
        let mut memory = vec![0; 0xc000];
        high_segment_page_tables(&mut memory);
        memory[0x4008..0x400c].copy_from_slice(&0x0000_7067u32.to_le_bytes());
        let mut code = vec![
            0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
            0x89, 0xc1, // mov ecx,eax
        ];
        code.extend_from_slice(return_bytes);
        memory[0x8301..0x8301 + code.len()].copy_from_slice(&code);
        memory[0x7000..0x7004].copy_from_slice(&TARGET.to_le_bytes());

        let mut native = quake_segment_cpu(ENTRY, true);
        let mut interp = quake_segment_cpu(ENTRY, true);
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let starts = [ENTRY, ENTRY + 5, RET];
        decode_segmented_fixture(&mut native, &mut native_bus, &starts);
        decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            assert_eq!(
                cpu.read_memory_sized(
                    bus,
                    SegmentIndex::Ss,
                    INITIAL_ESP,
                    OperandSize::Dword,
                    BusAccessKind::DataRead,
                )
                .unwrap(),
                TARGET
            );
            let linear = QUAKE_SEGMENT_BASE + INITIAL_ESP;
            assert_eq!(cpu.tlb.lookup(linear >> 12).unwrap().phys, 0x7000);
            assert!(cpu.jit_fast_map.has_read_mapping(linear, 0x7000));
            bus.trace.clear();
        }
        let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
        assert_eq!(block.span().instructions, 3);
        arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
        arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);

        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp)
        );
        assert_eq!(native.eflags(), interp.eflags());
        assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(native.registers.eip, TARGET);
        assert_eq!(native.registers.esp(), INITIAL_ESP + 4 + release);
    }
}

#[test]
fn finite_cs_ret_limit_exit_preserves_restart_state_and_faults_precisely() {
    for stack_physical in [0x7000, 0x000a_0000] {
        finite_cs_ret_limit_exit_case(stack_physical);
    }
}

fn finite_cs_ret_limit_exit_case(stack_physical: u32) {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const INITIAL_ESP: u32 = 0x2000;
    let mut memory = vec![0; 0x000b_0000];
    high_segment_page_tables(&mut memory);
    memory[0x4008..0x400c].copy_from_slice(&(stack_physical | 0x67).to_le_bytes());
    memory[0x8301..0x8309].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0xc3, // ret
    ]);
    let stack = stack_physical as usize;
    memory[stack..stack + 4].copy_from_slice(&(QUAKE_CS_LIMIT + 1).to_le_bytes());

    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, RET];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + INITIAL_ESP,
        stack_physical,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        true,
        false,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    assert_eq!(block.span().instructions, 3);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);
    let side_exits = native.perf_counters().jit_direct_side_exits;
    // The CS-limit refusal now names itself instead of landing in the `Other`
    // catch-all; `Other` has no Direct producer left at all.
    let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.registers.eip, RET);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
        1
    );

    let native_ret = native
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + RET, true)
        .unwrap();
    let interp_ret = interp
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + RET, true)
        .unwrap();
    let native_fault = native.execute_decoded(&native_ret, &mut native_bus);
    let interp_fault = interp.execute_decoded(&interp_ret, &mut interp_bus);
    for fault in [native_fault, interp_fault] {
        assert!(matches!(
            fault,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            })
        ));
    }
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.registers.eip, RET);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native_bus.memory, interp_bus.memory);
}

#[test]
fn nonflat_segment_limit_and_permission_fallbacks_are_transactional() {
    const ENTRY: u32 = 0x201;
    const STORE: u32 = ENTRY + 9;
    const TARGET: usize = 0x11000;
    let mut pristine = vec![0; 0x13000];
    pristine[ENTRY as usize..ENTRY as usize + 14].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0x89, 0xca, // mov edx,ecx
        0x89, 0x06, // mov [esi],eax
        0x89, 0xc3, // mov ebx,eax
        0xf4,
    ]);

    for (limit, access, emitted_limit_guard) in [(0x1002, 0x93, true), (u32::MAX, 0x91, false)] {
        let make_cpu = || {
            let mut cpu = flat_stack_cpu(ENTRY);
            cpu.registers.set_segment(
                SegmentIndex::Ds,
                SegmentRegister {
                    selector: 0x10,
                    base: 0x10000,
                    limit,
                    access,
                    default_size_32: true,
                },
            );
            cpu.registers.set_esi(0x1000);
            cpu
        };
        let mut native = make_cpu();
        let mut interp = make_cpu();
        let mut native_bus = TestBus::with_memory(pristine.clone());
        let mut interp_bus = TestBus::with_memory(pristine.clone());
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let starts = [ENTRY, ENTRY + 5, ENTRY + 7, STORE, STORE + 2];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        map_direct_page(
            &mut native,
            &mut native_bus,
            TARGET as u32,
            TARGET as u32,
            jit::fast_map::PagePermissions::UNPAGED,
            false,
            true,
        );
        let block = install_fixture_block(&mut native, ENTRY);
        for cpu in [&mut native, &mut interp] {
            cpu.registers.gpr.fill(0);
            cpu.registers.set_esi(0x1000);
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(ENTRY);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.core_clocks_so_far = 0;
        }
        let side_exits = native.perf_counters().jit_direct_side_exits;
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        for _ in 0..3 {
            interp.cycle(&mut interp_bus).unwrap();
        }
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp)
        );
        assert_eq!(native.registers.eip, STORE);
        assert_eq!(native.eflags(), interp.eflags());
        assert_eq!(&native_bus.memory[TARGET..TARGET + 4], &[0; 4]);
        assert_eq!(
            native.perf_counters().jit_direct_side_exits - side_exits,
            u64::from(emitted_limit_guard)
        );

        let native_decoded = native.decode_cache.get(STORE, true).unwrap();
        let interp_decoded = interp.decode_cache.get(STORE, true).unwrap();
        let native_fault = native.execute_decoded(&native_decoded, &mut native_bus);
        let interp_fault = interp.execute_decoded(&interp_decoded, &mut interp_bus);
        for fault in [native_fault, interp_fault] {
            assert!(matches!(
                fault,
                Err(InternalFault::Exception {
                    vector: 13,
                    error_code: Some(0)
                })
            ));
        }
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp)
        );
        assert_eq!(native.eflags(), interp.eflags());
        assert_eq!(&native_bus.memory[TARGET..TARGET + 4], &[0; 4]);
        assert_eq!(&interp_bus.memory[TARGET..TARGET + 4], &[0; 4]);
    }
}

#[test]
fn descriptor_change_selectively_recompiles_and_does_not_keep_a_stale_link() {
    const SOURCE: u32 = 0x101;
    const TARGET: u32 = 0x120;
    const END: u32 = 0x130;
    let mut memory = vec![0; 0x3000];
    memory[SOURCE as usize..SOURCE as usize + 10].copy_from_slice(&[
        0xa1, 0x00, 0x02, 0x00, 0x00, // mov eax,[0x200]
        0xe9, 0x15, 0x00, 0x00, 0x00, // jmp TARGET
    ]);
    memory[TARGET as usize..TARGET as usize + 11].copy_from_slice(&[
        0x8b, 0x0d, 0x04, 0x02, 0x00, 0x00, // mov ecx,[0x204]
        0x83, 0xc1, 0x01, // add ecx,1
        0xeb, 0x05, // jmp END
    ]);
    memory[0x200..0x208].copy_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
    memory[0x1200..0x1208].copy_from_slice(&[3, 0, 0, 0, 4, 0, 0, 0]);
    let mut cpu = flat_stack_cpu(SOURCE);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    decode_fixture(
        &mut cpu,
        &mut bus,
        &[SOURCE, SOURCE + 5, TARGET, TARGET + 6, TARGET + 9],
    );
    map_direct_page(
        &mut cpu,
        &mut bus,
        0,
        0,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    let old_source = install_fixture_block(&mut cpu, SOURCE);
    let old_target = install_fixture_block(&mut cpu, TARGET);
    assert!(cpu.jit_direct.has_linked_successor(old_source.id()));

    let mut changed_ds = cpu.registers.segment(SegmentIndex::Ds);
    changed_ds.base = 0x1000;
    cpu.registers.set_segment(SegmentIndex::Ds, changed_ds);
    map_direct_page(
        &mut cpu,
        &mut bus,
        0x1000,
        0x1000,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        false,
    );
    cpu.set_eip(SOURCE);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, old_source)
            .unwrap()
    );
    let source_key = old_source.span().key;
    assert!(matches!(
        cpu.jit_direct.probe(source_key),
        jit::direct::BlockProbe::Compile
    ));
    let source_compilation = jit::direct::compile(&mut cpu, SOURCE, true).unwrap();
    let source_id = cpu.jit_direct.install(&source_compilation).unwrap();
    let new_source = cpu.jit_direct.block(source_id).unwrap();
    assert!(!cpu.jit_direct.has_linked_successor(new_source.id()));

    cpu.set_eip(TARGET);
    assert!(
        !cpu.try_run_direct_block_for_test(&mut bus, old_target)
            .unwrap()
    );
    let target_key = old_target.span().key;
    assert!(matches!(
        cpu.jit_direct.probe(target_key),
        jit::direct::BlockProbe::Compile
    ));
    let target_compilation = jit::direct::compile(&mut cpu, TARGET, true).unwrap();
    let target_id = cpu.jit_direct.install(&target_compilation).unwrap();
    assert!(cpu.jit_direct.block(target_id).is_some());
    assert!(cpu.jit_direct.has_linked_successor(new_source.id()));

    cpu.registers.set_eax(0);
    cpu.registers.set_ecx(0);
    cpu.set_eip(SOURCE);
    let transfers = cpu.perf_counters().jit_direct_linked_transfers;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, new_source)
            .unwrap()
    );
    assert_eq!(cpu.registers.eip, END);
    assert_eq!(cpu.registers.eax(), 3);
    assert_eq!(cpu.registers.ecx(), 5);
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers,
        1
    );
}

/// Word admission is a POLICY now, not a persona constant, so this drives the policy explicitly
/// instead of leaning on a default.
///
/// It used to be named `..._only_for_586` and asserted 3 slots at I486 unconditionally. That was
/// true when the refusal was hard-coded; since the 486 measurement the default admits, and a test
/// that reads the default cannot say whether the default or the mechanism moved. Setting the flag
/// on both arms keeps it pinning the mechanism either way.
#[test]
fn word_renderer_slice_admission_follows_the_word_policy() {
    const ENTRY: u32 = 0x101;
    let code = [
        0x89, 0xc0, // mov eax,eax
        0x89, 0xc9, // mov ecx,ecx
        0x89, 0xd2, // mov edx,edx
        0x66, 0x89, 0xc0, // mov ax,ax
        0x89, 0xdb, // mov ebx,ebx
        0x89, 0xf6, // mov esi,esi
    ];
    let starts = [
        ENTRY,
        ENTRY + 2,
        ENTRY + 4,
        ENTRY + 6,
        ENTRY + 9,
        ENTRY + 11,
    ];

    for (mode, word_at_486, expected_instructions) in [
        // I486 refusing: the block stops AT the 66-prefixed slot, keeping the three before it.
        (GswMode::Gsw486, false, 3),
        // I486 admitting: identical to I586, which is the claim the 486 lift rests on.
        (GswMode::Gsw486, true, 6),
        // I586 is unconditional, so the flag must not move it in either direction.
        (GswMode::Gsw586, false, 6),
        (GswMode::Gsw586, true, 6),
    ] {
        let mut memory = vec![0; 0x1000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_stack_cpu(ENTRY);
        cpu.set_mode(mode);
        cpu.set_word_operands_at_486(word_at_486);
        let mut bus = TestBus::with_memory(memory);
        decode_fixture(&mut cpu, &mut bus, &starts);

        let block = install_fixture_block(&mut cpu, ENTRY);
        assert_eq!(
            block.span().instructions,
            expected_instructions,
            "{mode:?} word_at_486={word_at_486}"
        );
    }
}

#[test]
fn quake_word_renderer_families_match_interpreter_state_flags_memory_and_timing() {
    const ENTRY: u32 = 0x101;
    const DATA: u32 = 0x3000;
    let code = [
        0x66, 0x89, 0xd8, // mov ax,bx
        0x66, 0x8b, 0xf8, // mov di,ax
        0x66, 0x89, 0x0d, 0x00, 0x30, 0x00, 0x00, // mov word [DATA],cx
        0x66, 0x8b, 0x15, 0x00, 0x30, 0x00, 0x00, // mov dx,word [DATA]
        0x66, 0xff, 0x05, 0x02, 0x30, 0x00, 0x00, // inc word [DATA+2]
        0x66, 0xff, 0x0d, 0x02, 0x30, 0x00, 0x00, // dec word [DATA+2]
        0x66, 0x4b, // dec bx
        0x66, 0xff, 0xc1, // inc cx through FF /0
        0x66, 0x39, 0x1d, 0x00, 0x30, 0x00, 0x00, // cmp word [DATA],bx
        0x72, 0x0b, // jb final HLT, not taken when the preceding CMP is correct
        0x66, 0x3b, 0x1d, 0x00, 0x30, 0x00, 0x00, // cmp bx,word [DATA]
        0x89, 0xf6, // mov esi,esi keeps the comparison flags live
        0x89, 0xf6, // second filler keeps the comparison block independently compilable
        0xf4,
    ];
    let starts = [
        ENTRY,
        ENTRY + 3,
        ENTRY + 6,
        ENTRY + 13,
        ENTRY + 20,
        ENTRY + 27,
        ENTRY + 34,
        ENTRY + 36,
        ENTRY + 39,
        ENTRY + 46,
        ENTRY + 48,
        ENTRY + 55,
        ENTRY + 57,
    ];
    let mut memory = vec![0; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[DATA as usize + 2..DATA as usize + 4].copy_from_slice(&0xffffu16.to_le_bytes());

    let mut direct = flat_stack_cpu(ENTRY);
    let mut interpreter = flat_stack_cpu(ENTRY);
    let mut direct_bus = TestBus::with_memory(memory.clone());
    let mut interpreter_bus = TestBus::with_memory(memory);
    for bus in [&mut direct_bus, &mut interpreter_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    decode_fixture(&mut direct, &mut direct_bus, &starts);
    decode_fixture(&mut interpreter, &mut interpreter_bus, &starts);
    for (cpu, bus) in [
        (&mut direct, &mut direct_bus),
        (&mut interpreter, &mut interpreter_bus),
    ] {
        map_direct_page(
            cpu,
            bus,
            DATA,
            DATA,
            jit::fast_map::PagePermissions::UNPAGED,
            true,
            true,
        );
        cpu.registers.set_ecx(0xaaaa_1234);
        cpu.registers.set_edx(0xbbbb_0000);
        cpu.registers.set_ebx(0xcccc_1200);
        cpu.registers.set_eax(0xdddd_0000);
        cpu.registers.set_edi(0xeeee_0000);
        cpu.registers.eflags = 0x203;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
    }
    let first = install_fixture_block(&mut direct, ENTRY);
    let first_compare = install_fixture_block(&mut direct, ENTRY + 39);
    let second_compare = install_fixture_block(&mut direct, ENTRY + 48);
    assert_eq!(first.span().instructions, 8);
    assert_eq!(first.word_reads(), 3);
    assert_eq!(first.word_stores(), 3);
    assert_eq!(first_compare.span().instructions, 2);
    assert_eq!(first_compare.word_reads(), 1);
    assert_eq!(first_compare.word_stores(), 0);
    assert_eq!(second_compare.span().instructions, 3);
    assert_eq!(second_compare.word_reads(), 1);
    assert_eq!(second_compare.word_stores(), 0);

    assert!(
        direct
            .try_run_direct_block_for_test(&mut direct_bus, first)
            .unwrap()
    );
    for _ in 0..13 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }

    assert_eq!(
        crate::tests::settled_registers(&direct),
        crate::tests::settled_registers(&interpreter)
    );
    assert_eq!(direct.eflags(), interpreter.eflags());
    assert_eq!(direct_bus.memory, interpreter_bus.memory);
    assert_eq!(direct.elapsed_clocks, interpreter.elapsed_clocks);
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks()
    );
    assert_eq!(direct.registers.edx(), 0xbbbb_1234);
    assert_eq!(direct.registers.ebx(), 0xcccc_11ff);
    assert_eq!(direct.registers.ecx(), 0xaaaa_1235);
    assert_eq!(direct.registers.eax(), 0xdddd_1200);
    assert_eq!(direct.registers.edi(), 0xeeee_1200);
    assert_eq!(
        &direct_bus.memory[DATA as usize..DATA as usize + 4],
        &[0x34, 0x12, 0xff, 0xff]
    );
}

const RT_ENTRY_A: u32 = 0x101;
const RT_ENTRY_B_NORMAL: u32 = 0x201;
// `key_for` returns None for physical addresses in 0xA0000..0x100000, so an entry placed here is
// never admitted to the direct backend and stays on the interpreter forever.
const RT_ENTRY_B_EXCLUDED: u32 = 0x000a_1000;

// Two blocks, each one register ALU op followed by an unconditional near jump to the other, so the
// guest is an endless A -> B -> A loop. Bodies are kept to a single ALU op so that when B falls to
// the interpreter (the UNRESOLVED config) its interpret-vs-native residual stays minimal.
fn roundtrip_pair_memory(entry_b: u32) -> Vec<u8> {
    let mut memory = vec![0; (entry_b as usize + 16).max(0x1000)];
    let write_block = |memory: &mut Vec<u8>, entry: u32, alu: [u8; 3], target: u32| {
        let base = entry as usize;
        memory[base..base + 3].copy_from_slice(&alu);
        let jmp_at = entry + 3;
        memory[jmp_at as usize] = 0xe9;
        let rel = i64::from(target) - (i64::from(jmp_at) + 5);
        memory[jmp_at as usize + 1..jmp_at as usize + 5]
            .copy_from_slice(&(rel as i32).to_le_bytes());
    };
    // A: add eax,1 ; jmp B
    write_block(&mut memory, RT_ENTRY_A, [0x83, 0xc0, 0x01], entry_b);
    // B: add edx,1 ; jmp A
    write_block(&mut memory, entry_b, [0x83, 0xc2, 0x01], RT_ENTRY_A);
    memory
}

fn roundtrip_cpu(entry_b: u32) -> (CpuGsw, TestBus) {
    let mut cpu = flat_stack_cpu(RT_ENTRY_A);
    cpu.registers.gpr.fill(0);
    cpu.registers.eflags = 0x2; // IF clear: no interrupt-transition run breaks.
    cpu.pending_flags = PendingFlags::default();
    cpu.jit_direct.set_admission_heat_for_test(1);
    cpu.set_jit_auto_admit(true);
    let mut bus = TestBus::with_memory(roundtrip_pair_memory(entry_b));
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    // Uniform native fetches take the trace-free native path (no per-entry trace allocation), so
    // the measured linked-transfer cost reflects steady-state native dispatch rather than the
    // per-block trace bookkeeping.
    bus.uniform_native_fetches = true;
    // Production runs with bus tracing off when the JIT is active; the TestBus default (Full)
    // would make every interpreted instruction in the UNRESOLVED config push BusCycles into the
    // trace VecDeque, a cost production never pays.
    bus.trace.set_tracing_mode(TracingMode::Off);
    (cpu, bus)
}

fn roundtrip_median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    }
}

/// Measure, on the production `run_budgeted` dispatch path, the wall cost of an unresolved native
/// exit that returns to the interpreter and re-dispatches, relative to a native linked transfer
/// that stays in compiled code. This replaces the 2011-literature inference the superblock-JIT
/// performance thesis rested on with a number taken on the real path.
///
/// The guest is a two-block A <-> B loop (each block one ALU op plus a near jump to the other) run
/// at equal guest work in two configurations:
///   LINKED     - both blocks admitted; the A<->B edges link natively (no return to Rust per edge).
///   UNRESOLVED - block A admitted, block B placed in the `key_for` physical-exclusion window so it
///                is never admitted; every A->B edge is an unresolved static-unbound exit and B
///                runs on the interpreter.
///
/// The difference isolates the round-trip cost: the block bodies and per-block accounting appear in
/// both configs and cancel. B interprets instead of running natively in UNRESOLVED, which does not
/// fully cancel, so the printed unresolved number is a small OVERESTIMATE by B's interpret-vs-native
/// delta (B is one ALU op plus a jump to keep that residual small).
///
/// Normalization is counter-based, never by intended iteration count. One unresolved exit advances
/// the guest by one full A->B->A round (A native, B interpreted); the equal-work linked cost of
/// that round is two linked transfers (the A->B and B->A edges). Hence the headline subtracts twice
/// the per-linked-transfer time from the per-unresolved-exit time.
#[ignore = "timing probe; run explicitly with --ignored roundtrip --nocapture"]
#[test]
fn roundtrip_unresolved_exit_versus_linked_transfer_ns() {
    if cfg!(debug_assertions) {
        panic!("run this probe with --release; a debug build prints a garbage gate number");
    }
    // CAP bounds one native chain sweep in the LINKED config (the UNRESOLVED config breaks at B's
    // non-continuable jump every round regardless). TARGET is the per-sample counter goal; WARM
    // drives each config to steady state before the timed samples. In UNRESOLVED, run_budgeted
    // returns once per round and the test loop re-invokes it, standing in for the machine batch
    // loop; that slightly under-counts the production round cost (the conservative direction).
    const CAP: u64 = 2_000_000;
    const TARGET: u64 = 2_000_000;
    const WARM: u64 = 50_000;
    const SAMPLES: usize = 5;

    // ---- LINKED: both A and B admitted, A<->B edges link natively. ----
    let (mut cpu, mut bus) = roundtrip_cpu(RT_ENTRY_B_NORMAL);
    let warm_from = cpu.perf_counters().jit_direct_linked_transfers;
    while cpu.perf_counters().jit_direct_linked_transfers - warm_from < WARM {
        cpu.run_budgeted(&mut bus, CAP).unwrap();
    }
    assert!(
        cpu.jit_direct.len() >= 2,
        "LINKED config must admit both A and B, got {}",
        cpu.jit_direct.len()
    );

    let mut linked_samples = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let linked0 = cpu.perf_counters().jit_direct_linked_transfers;
        let unresolved0 = cpu.perf_counters().jit_direct_unresolved_exits;
        let started = std::time::Instant::now();
        loop {
            cpu.run_budgeted(&mut bus, CAP).unwrap();
            if cpu.perf_counters().jit_direct_linked_transfers - linked0 >= TARGET {
                break;
            }
        }
        let elapsed_ns = started.elapsed().as_nanos() as f64;
        let linked = cpu.perf_counters().jit_direct_linked_transfers - linked0;
        let unresolved = cpu.perf_counters().jit_direct_unresolved_exits - unresolved0;
        assert!(
            linked >= TARGET,
            "LINKED sample {sample} under target: {linked}"
        );
        assert!(
            unresolved.saturating_mul(100) < linked,
            "LINKED sample {sample} must stay native, saw unresolved={unresolved} linked={linked}"
        );
        linked_samples.push(elapsed_ns / linked as f64);
    }

    // ---- UNRESOLVED: B excluded from admission, so every A->B edge round-trips through Rust. ----
    let (mut cpu, mut bus) = roundtrip_cpu(RT_ENTRY_B_EXCLUDED);
    let warm_from = cpu.perf_counters().jit_direct_unresolved_exits;
    while cpu.perf_counters().jit_direct_unresolved_exits - warm_from < WARM {
        cpu.run_budgeted(&mut bus, CAP).unwrap();
    }
    // B's entry is inside the JIT physical-exclusion window, so it can never be admitted: `key_for`
    // returns None for it regardless of hotness. The counter proofs below (linked == 0, every
    // unresolved exit static-unbound) confirm B never becomes a native or linked target at runtime.
    assert!(
        jit::direct::key_for(&cpu, RT_ENTRY_B_EXCLUDED, true).is_none(),
        "B's entry must fall in the key_for physical-exclusion window"
    );

    let mut unresolved_samples = Vec::with_capacity(SAMPLES);
    for sample in 0..SAMPLES {
        let linked0 = cpu.perf_counters().jit_direct_linked_transfers;
        let unresolved0 = cpu.perf_counters().jit_direct_unresolved_exits;
        let entries0 = cpu.perf_counters().jit_direct_entries;
        let static_unbound0 = cpu.perf_counters().jit_direct_unresolved_static_unbound;
        let started = std::time::Instant::now();
        loop {
            cpu.run_budgeted(&mut bus, CAP).unwrap();
            if cpu.perf_counters().jit_direct_unresolved_exits - unresolved0 >= TARGET {
                break;
            }
        }
        let elapsed_ns = started.elapsed().as_nanos() as f64;
        let linked = cpu.perf_counters().jit_direct_linked_transfers - linked0;
        let unresolved = cpu.perf_counters().jit_direct_unresolved_exits - unresolved0;
        let entries = cpu.perf_counters().jit_direct_entries - entries0;
        let static_unbound =
            cpu.perf_counters().jit_direct_unresolved_static_unbound - static_unbound0;
        assert_eq!(
            linked, 0,
            "UNRESOLVED sample {sample} must never link a transfer"
        );
        assert!(
            unresolved >= TARGET,
            "UNRESOLVED sample {sample} under target: {unresolved}"
        );
        assert_eq!(
            static_unbound, unresolved,
            "sample {sample}: every unresolved exit must be a static-unbound A->B edge"
        );
        assert!(
            entries >= unresolved && entries - unresolved <= unresolved / 100,
            "sample {sample}: expect one native A entry per unresolved round, entries={entries} unresolved={unresolved}"
        );
        unresolved_samples.push(elapsed_ns / unresolved as f64);
    }

    let linked_ns_per_transfer = roundtrip_median(linked_samples);
    let unresolved_ns_per_exit = roundtrip_median(unresolved_samples);
    // One unresolved exit == one A->B->A round == two linked transfers of equal guest work. The
    // difference is a small overestimate of the pure round-trip cost by B's interpret-vs-native
    // delta (see the doc comment).
    let unresolved_minus_linked_ns_per_exit = unresolved_ns_per_exit - 2.0 * linked_ns_per_transfer;
    println!(
        "roundtrip unresolved_minus_linked_ns_per_exit={unresolved_minus_linked_ns_per_exit:.3} unresolved_ns_per_exit={unresolved_ns_per_exit:.3} linked_ns_per_transfer={linked_ns_per_transfer:.3}"
    );
    assert!(
        unresolved_minus_linked_ns_per_exit > 0.0,
        "an unresolved round-trip must cost more than staying in linked native code"
    );
}

/// Eight pairwise-distinct byte lanes, one per guest register, so that reading the WRONG register
/// or the WRONG lane of the right register both produce a value no correct emitter could.
///
/// This is not decoration. The natural mutation here is dropping the `src >= 4` lane adjustment,
/// which makes a high-byte source read `home(4..=7)`, that is guest ESP, EBP, ESI and EDI. The
/// precedent fixture in this file seeds with `gpr.fill(0xdead_beef)` and overrides only two
/// registers, under which every register's byte 1 is 0xbe and that mutation SURVIVES. Every lane
/// below is distinct, and the test asserts that rather than trusting it.
const LANE_SEEDS: [u32; 8] = [
    0x1000_2301, // EAX: AL=0x01 AH=0x23
    0x1100_4502, // ECX: CL=0x02 CH=0x45
    0x1200_6703, // EDX: DL=0x03 DH=0x67
    0x1300_8904, // EBX: BL=0x04 BH=0x89
    0x0000_ab05, // ESP: must stay a usable stack pointer, so the high half is 0
    0x1500_cd06, // EBP
    0x1600_ef07, // ESI
    0x1700_fe08, // EDI
];

fn seed_lanes(cpu: &mut CpuGsw) {
    for (index, value) in LANE_SEEDS.into_iter().enumerate() {
        cpu.registers.gpr[index] = value;
    }
}

/// What the guest's byte register `index` holds under LANE_SEEDS: the low byte for 0..=3, the high
/// byte of `index - 4` for 4..=7, exactly as the interpreter's `read_gpr8` defines it.
fn lane_byte(index: u8) -> u8 {
    let value = LANE_SEEDS[usize::from(index & 3)];
    if index < 4 {
        value as u8
    } else {
        (value >> 8) as u8
    }
}

#[test]
fn lane_seeds_are_pairwise_distinct() {
    // The discrimination the batteries below rely on, asserted structurally instead of assumed.
    let bytes: Vec<u8> = (0..8).map(lane_byte).collect();
    for i in 0..bytes.len() {
        for j in (i + 1)..bytes.len() {
            assert_ne!(
                bytes[i], bytes[j],
                "byte registers must differ or the high-byte battery cannot distinguish a wrong \
                 lane from a wrong register"
            );
        }
    }
    let words: Vec<u32> = LANE_SEEDS.iter().map(|seed| seed & 0xffff).collect();
    for (i, left) in words.iter().enumerate() {
        for right in &words[i + 1..] {
            assert_ne!(left, right, "word lanes must differ");
        }
    }
}

/// `movzx/movsx <dst32>, <src8|src16>`, register form: 0F <op> /r with mod=11.
fn movzx_reg_case(signed: bool, word: bool, dst: u8, src: u8) -> Vec<u8> {
    let opcode: u8 = match (signed, word) {
        (false, false) => 0xb6,
        (false, true) => 0xb7,
        (true, false) => 0xbe,
        (true, true) => 0xbf,
    };
    let modrm = 0b1100_0000 | ((dst & 7) << 3) | (src & 7);
    let mut code = vec![0x0f, opcode, modrm];
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0xf6, 0xf4]);
    code
}

/// THE HIGH-BYTE BATTERY. Every byte source 0..=7 for both byte opcodes, differential against the
/// interpreter and pinned against an independently computed expectation.
#[test]
fn movzx_register_forms_read_the_right_byte_lane() {
    const ENTRY: u32 = 0x101;
    for signed in [false, true] {
        for src in 0u8..8 {
            let code = movzx_reg_case(signed, false, 1, src);
            let mut memory = vec![0; 0x0004_0000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

            let mut native = fresh();
            let mut interp = fresh();
            make_data_segments_flat(&mut native);
            make_data_segments_flat(&mut interp);
            let mut native_bus = TestBus::with_memory(memory.clone());
            let mut interp_bus = TestBus::with_memory(memory);
            for bus in [&mut native_bus, &mut interp_bus] {
                bus.direct_pages_enabled = true;
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.uniform_native_fetches = true;
            }
            let starts = [ENTRY, ENTRY + 3, ENTRY + 5];
            decode_fixture(&mut native, &mut native_bus, &starts);
            decode_fixture(&mut interp, &mut interp_bus, &starts);
            let block = install_fixture_block(&mut native, ENTRY);

            for cpu in [&mut native, &mut interp] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                seed_lanes(cpu);
                cpu.registers.eflags = 0x0296;
                cpu.pending_flags = PendingFlags::default();
                cpu.registers.eip = ENTRY;
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.core_clocks_so_far = 0;
            }
            let label = format!("signed={signed} src={src}");
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "{label}: must run natively"
            );
            for _ in 0..3 {
                interp.cycle(&mut interp_bus).unwrap();
            }
            assert_eq!(
                crate::tests::settled_registers(&native),
                crate::tests::settled_registers(&interp),
                "{label}: registers"
            );
            assert_eq!(
                native.eflags(),
                interp.eflags(),
                "{label}: eflags untouched"
            );
            assert_eq!(
                native.elapsed_clocks, interp.elapsed_clocks,
                "{label}: core clocks"
            );
            // Pinned independently of the interpreter, so both sides being wrong the same way
            // still fails.
            let raw = lane_byte(src);
            let expected = if signed {
                raw as i8 as i32 as u32
            } else {
                u32::from(raw)
            };
            assert_eq!(native.registers.ecx(), expected, "{label}: extended value");
        }
    }
}

/// Word sources, sign polarity, a destination index above 3, and dst == src aliasing including the
/// high-byte case where the source's base home IS the destination.
#[test]
fn movzx_register_forms_cover_widths_polarity_and_aliasing() {
    const ENTRY: u32 = 0x101;
    // (word, dst, src). ESP is never a destination: overwriting it mid-block would leave the
    // fixture without a stack, which is a fixture bug rather than a lowering test.
    let cases: &[(bool, u8, u8)] = &[
        (true, 3, 1),  // movzx ebx, cx: the shape that is 90 percent of the measured cell
        (true, 0, 0),  // dst == src, word
        (true, 6, 2),  // destination index above 3
        (false, 0, 4), // movzx eax, ah: dst home IS the high-byte source's base
        (false, 3, 7), // movzx ebx, bh: same, on a different register
        (false, 6, 5), // destination above 3 with a high-byte source
    ];
    for signed in [false, true] {
        for &(word, dst, src) in cases {
            let code = movzx_reg_case(signed, word, dst, src);
            let mut memory = vec![0; 0x0004_0000];
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

            let mut native = fresh();
            let mut interp = fresh();
            make_data_segments_flat(&mut native);
            make_data_segments_flat(&mut interp);
            let mut native_bus = TestBus::with_memory(memory.clone());
            let mut interp_bus = TestBus::with_memory(memory);
            for bus in [&mut native_bus, &mut interp_bus] {
                bus.direct_pages_enabled = true;
                bus.direct_page_clocks = true;
                bus.report_batch_clocks = true;
                bus.uniform_native_fetches = true;
            }
            let starts = [ENTRY, ENTRY + 3, ENTRY + 5];
            decode_fixture(&mut native, &mut native_bus, &starts);
            decode_fixture(&mut interp, &mut interp_bus, &starts);
            let block = install_fixture_block(&mut native, ENTRY);
            for cpu in [&mut native, &mut interp] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                seed_lanes(cpu);
                cpu.registers.eflags = 0x0296;
                cpu.pending_flags = PendingFlags::default();
                cpu.registers.eip = ENTRY;
                cpu.elapsed_clocks = 0;
                cpu.timing_rem = 0;
                cpu.core_clocks_so_far = 0;
            }
            let label = format!("signed={signed} word={word} dst={dst} src={src}");
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "{label}: must run natively"
            );
            for _ in 0..3 {
                interp.cycle(&mut interp_bus).unwrap();
            }
            assert_eq!(
                crate::tests::settled_registers(&native),
                crate::tests::settled_registers(&interp),
                "{label}: registers"
            );
            assert_eq!(
                native.eflags(),
                interp.eflags(),
                "{label}: eflags untouched"
            );

            let expected = if word {
                let raw = (LANE_SEEDS[usize::from(src)] & 0xffff) as u16;
                if signed {
                    raw as i16 as i32 as u32
                } else {
                    u32::from(raw)
                }
            } else {
                let raw = lane_byte(src);
                if signed {
                    raw as i8 as i32 as u32
                } else {
                    u32::from(raw)
                }
            };
            assert_eq!(
                native.registers.gpr[usize::from(dst)],
                expected,
                "{label}: all 32 destination bits must be replaced, never merged"
            );
        }
    }
}

/// Registration: this form touches NO memory, so a copy-paste of the memory variant's declarations
/// is caught here. And raw_clocks is 3, not the DirectKind default of 2.
#[test]
fn movzx_register_form_declares_no_memory_and_three_clocks() {
    const ENTRY: u32 = 0x101;
    let code = movzx_reg_case(false, true, 3, 1);
    let mut memory = vec![0; 0x0004_0000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = fresh();
    make_data_segments_flat(&mut cpu);
    cpu.registers.eip = ENTRY;
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    let starts = [ENTRY, ENTRY + 3, ENTRY + 5];
    decode_fixture(&mut cpu, &mut bus, &starts);
    let block = install_fixture_block(&mut cpu, ENTRY);
    assert_eq!(block.span().instructions, 3, "whole block admitted");
    assert_eq!(block.byte_reads(), 0, "no byte read");
    assert_eq!(block.word_reads(), 0, "no word read");
    assert_eq!(block.dword_reads(), 0, "no dword read");
    assert!(!block.has_wide_accesses(), "no wide access");
    // clocks(3) for the MOVZX per every interpreter arm, plus 2 each for the two register moves.
    assert_eq!(block.raw_clocks(), 3 + 2 + 2, "charged core clocks");
}

#[test]
fn movzx_register_forms_are_lowered() {
    // The positive control for the 66-prefixed negative list, and the only test that catches the
    // classify arm being keyed on the low opcode byte, where the u8 truncation makes it
    // unreachable for a two-byte opcode.
    const ENTRY: u32 = 0x101;
    for (signed, word) in [(false, false), (false, true), (true, false), (true, true)] {
        let code = movzx_reg_case(signed, word, 3, 1);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = fresh();
        make_data_segments_flat(&mut cpu);
        cpu.registers.eip = ENTRY;
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        let starts = [ENTRY, ENTRY + 3, ENTRY + 5];
        decode_fixture(&mut cpu, &mut bus, &starts);
        let outcome = jit::direct::compile(&mut cpu, ENTRY, true);
        let instructions = outcome
            .is_some()
            .then(|| outcome.unwrap().span.instructions);
        assert_eq!(
            instructions,
            Some(3),
            "signed={signed} word={word} register form must admit and carry the whole block"
        );
    }
}

/// The sibling of `finite_cs_ret_limit_exit_case`, and the case that fixture missed by one seed.
///
/// That one always seeds a return address ABOVE the CS limit, so the emitted `ja` jumps past the
/// mode13 read completion every time. This one seeds a VALID return address, so the completion
/// runs while the return target is still live in a register.
///
/// It exists because the completion clobbers RDX: `emit_dynamic_increment` is `mov RDX, 1`
/// followed by an add. The RET arm was the only emitter site holding a live value in RDX across
/// it, so before this was fixed a near RET whose stack read landed on a mode13 page returned to
/// EIP 1. The `stack_physical` loop is what makes the mode13 branch actually taken; on an
/// ordinary RAM page the completion's guarded body is skipped and the bug is invisible.
#[test]
fn ret_through_a_mode13_stack_page_returns_to_the_popped_address() {
    for stack_physical in [0x7000, 0x000a_0000] {
        finite_cs_ret_valid_target_case(stack_physical);
    }
}

fn finite_cs_ret_valid_target_case(stack_physical: u32) {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const INITIAL_ESP: u32 = 0x2000;
    const RETURN_TO: u32 = 0x4321;
    let mut memory = vec![0; 0x000b_0000];
    high_segment_page_tables(&mut memory);
    memory[0x4008..0x400c].copy_from_slice(&(stack_physical | 0x67).to_le_bytes());
    memory[0x8301..0x8309].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0xc3, // ret
    ]);
    let stack = stack_physical as usize;
    memory[stack..stack + 4].copy_from_slice(&RETURN_TO.to_le_bytes());

    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, RET];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + INITIAL_ESP,
        stack_physical,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        true,
        false,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    assert_eq!(block.span().instructions, 3);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }
    assert_eq!(
        native.registers.eip, RETURN_TO,
        "the RET must return to the popped address, not to a counter constant"
    );
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.registers.esp(), INITIAL_ESP + 4);
    // No clock comparison here. A COMPLETED block and three interpreter cycles do not present
    // the same accounting boundary (the block charges its own total, the interpreter charges
    // per cycle plus the post-RET prefetch), and the difference shows on an ordinary RAM stack
    // too, where the code this test exists for is not even reached. RET timing on both the
    // completing and the exiting path is already pinned by the sibling case above and by
    // `direct_family_core_and_bus_timing_matches_interpreter_in_486_and_586_modes`. This test
    // is here for the returned ADDRESS.
}

/// The Word-width twin of `ret_through_a_mode13_stack_page_returns_to_the_popped_address`, and
/// the only thing that can reach the 16-bit RET's mode13 completion.
///
/// The completion clobbers RDX, and at Word the increment loads `1 << 32`, whose low half is
/// ZERO. So without the re-load a 16-bit RET off a mode13 stack page returns to EIP 0 rather
/// than to EIP 1 as the 32-bit form did: the same defect with a quieter symptom.
///
/// It needs three things at once, which is why no existing helper provides it: a mode13 stack
/// page, a 16-bit stack, and a return address that PASSES the CS limit so the completion is
/// reached at all.
#[test]
fn word_ret_through_a_mode13_stack_page_returns_to_the_popped_address() {
    const ENTRY: u32 = 0x301;
    const RET: u32 = ENTRY + 7;
    const INITIAL_ESP: u32 = 0x2000;
    const RETURN_TO: u32 = 0x4321;
    const STACK_PHYSICAL: u32 = 0x000a_0000;
    let mut memory = vec![0; 0x000b_0000];
    high_segment_page_tables(&mut memory);
    memory[0x4008..0x400c].copy_from_slice(&(STACK_PHYSICAL | 0x67).to_le_bytes());
    memory[0x8301..0x830a].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0x66, 0xc3, // ret at Word operand size
    ]);
    let stack = STACK_PHYSICAL as usize;
    memory[stack..stack + 2].copy_from_slice(&(RETURN_TO as u16).to_le_bytes());

    let sixteen_bit_stack = |cpu: &mut CpuGsw| {
        let mut ss = cpu.registers.segment(SegmentIndex::Ss);
        ss.default_size_32 = false;
        cpu.registers.set_segment(SegmentIndex::Ss, ss);
    };
    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    sixteen_bit_stack(&mut native);
    sixteen_bit_stack(&mut interp);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, RET];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + INITIAL_ESP,
        STACK_PHYSICAL,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        true,
        false,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    assert_eq!(block.span().instructions, 3);
    arm_stack_fixture(&mut native, ENTRY, INITIAL_ESP);
    arm_stack_fixture(&mut interp, ENTRY, INITIAL_ESP);

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interp.cycle(&mut interp_bus).unwrap();
    }
    assert_eq!(
        native.registers.eip, RETURN_TO,
        "the 16-bit RET must return to the popped word, not to a cleared register"
    );
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native.registers.esp(), INITIAL_ESP + 2);
}

/// The chain-quota memo is keyed on `has_x87` and on the bus's cost-dial epoch. Quake cannot
/// test the invalidation: a single-persona run never changes a dial, so byte identity on the
/// corpus would hold whether the clear existed or not.
///
/// Two independent mechanisms, both pinned here. The eager one is the clear in
/// `BlockCache::clear`, placed ABOVE its empty-cache early return, which matters because with no
/// block installed that return is taken and `reset_storage` never runs. The lazy one is the
/// epoch, which covers a dial moving without the CPU's mode moving at all.
#[test]
fn chain_quota_memo_clears_on_a_mode_change_even_when_no_block_is_cached() {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    assert_eq!(cpu.jit_direct.global_block_upper_cached(0, 7), 0);

    cpu.jit_direct.set_global_block_upper_cached(0, 7, 12_345);
    cpu.jit_direct.set_global_block_upper_cached(1, 7, 67_890);
    assert_eq!(cpu.jit_direct.global_block_upper_cached(0, 7), 12_345);
    assert_eq!(cpu.jit_direct.global_block_upper_cached(1, 7), 67_890);

    // A different bus epoch invalidates without any CPU involvement at all.
    assert_eq!(cpu.jit_direct.global_block_upper_cached(0, 8), 0);

    // And no block was ever installed, so `clear()` takes the empty-cache early return.
    cpu.set_mode(GswMode::Gsw486);
    assert_eq!(
        cpu.jit_direct.global_block_upper_cached(0, 7),
        0,
        "a mode change must drop the chain-quota memo even when the block cache is empty"
    );
}

/// One CPU holding two installed blocks that must not share a memo slot: an x87 block (FLD1 then
/// FSTP m64, so both `weighted_fp_clocks` and the dword-store count are nonzero) and an integer
/// block of the same instruction count with no memory traffic at all. Returns them in that order.
///
/// Modelled on `fstp_m64_matches_the_interpreter_in_ram_and_in_the_aperture` below, minus the
/// interpreter differential: these fixtures are never executed, only priced.
fn install_x87_and_integer_blocks(
    mode: GswMode,
) -> (
    CpuGsw,
    TestBus,
    jit::direct::CompiledBlock,
    jit::direct::CompiledBlock,
) {
    const X87_ENTRY: u32 = 0x101;
    const INT_ENTRY: u32 = 0x201;
    const RAM: u32 = 0x0003_0000;

    let mut memory = vec![0; 0x000b_0000];
    let mut x87_code = vec![0xd9u8, 0xe8, 0xdd, 0x9e]; // fld1 ; fstp qword [esi+disp32]
    x87_code.extend_from_slice(&RAM.to_le_bytes());
    x87_code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    memory[X87_ENTRY as usize..X87_ENTRY as usize + x87_code.len()].copy_from_slice(&x87_code);
    let int_code = [
        0x83u8, 0xc0, 0x01, // add eax,1
        0x83, 0xc0, 0x01, // add eax,1
        0x89, 0xf6, // mov esi,esi
        0xf4, // hlt
    ];
    memory[INT_ENTRY as usize..INT_ENTRY as usize + int_code.len()].copy_from_slice(&int_code);

    // `fresh()` with the mode chosen up front: `set_mode` reloads the segments, so it has to run
    // before the flat-segment setup rather than after it.
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
    }
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.registers.eip = 0x100;
    make_data_segments_flat(&mut cpu);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.report_batch_clocks = true;
    bus.uniform_native_fetches = true;
    let starts = [
        X87_ENTRY,
        X87_ENTRY + 2,
        X87_ENTRY + 8,
        INT_ENTRY,
        INT_ENTRY + 3,
        INT_ENTRY + 6,
    ];
    decode_fixture(&mut cpu, &mut bus, &starts);
    map_direct_page(
        &mut cpu,
        &mut bus,
        RAM,
        RAM,
        jit::fast_map::PagePermissions::UNPAGED,
        true,
        true,
    );
    cpu.fpu = X87::default();
    let x87_block = install_fixture_block(&mut cpu, X87_ENTRY);
    let int_block = install_fixture_block(&mut cpu, INT_ENTRY);
    assert!(x87_block.has_x87(), "the x87 fixture must be an x87 block");
    assert!(
        !int_block.has_x87(),
        "the integer fixture must not be an x87 block"
    );
    (cpu, bus, x87_block, int_block)
}

/// N4: the per-entry `iteration_upper` memo must never disagree with a fresh computation, and two
/// blocks whose bounds genuinely differ must not collide in one slot. The x87/integer pair is the
/// widest split available: the float block carries a `weighted_fp_clocks` term and two dword
/// stores that the integer block has neither of.
///
/// Both a cold and a warm read are asserted, so a memo that never stored and a memo that stored
/// the wrong block's value both fail here.
#[test]
fn iteration_upper_memo_matches_a_fresh_recompute_for_x87_and_integer_blocks() {
    let (mut cpu, bus, x87_block, int_block) = install_x87_and_integer_blocks(GswMode::Gsw586);

    let x87_fresh = cpu.recompute_iteration_upper_for_test(&bus, &x87_block);
    let int_fresh = cpu.recompute_iteration_upper_for_test(&bus, &int_block);
    assert_ne!(
        x87_fresh, int_fresh,
        "the fixture pair must price differently or this test cannot see a slot collision"
    );

    // Pass 0 is the cold fill, pass 1 the memo hit. Both must agree with the fresh value.
    for pass in 0..2 {
        assert_eq!(
            cpu.iteration_upper_for_test(&bus, &x87_block),
            x87_fresh,
            "x87 block, pass {pass}"
        );
        assert_eq!(
            cpu.iteration_upper_for_test(&bus, &int_block),
            int_fresh,
            "integer block, pass {pass}"
        );
    }
}

/// The other half of the memo key. Two independent invalidations, both pinned: the bus's cost-dial
/// epoch, which covers a dial moving with no CPU involvement at all, and the mode change, which
/// reaches `BlockCache::clear` and drops the whole table with the blocks it described.
///
/// Neither fixture can prove this: a single-persona corpus run never changes a dial, so byte
/// identity would hold whether the invalidation existed or not. Same reasoning as
/// `chain_quota_memo_clears_on_a_mode_change_even_when_no_block_is_cached` above.
#[test]
fn iteration_upper_memo_is_dropped_by_a_dial_epoch_change_and_by_a_mode_change() {
    let (mut cpu, _bus, x87_block, int_block) = install_x87_and_integer_blocks(GswMode::Gsw586);
    let x87_id = x87_block.id();
    let int_id = int_block.id();

    assert_eq!(cpu.jit_direct.iteration_upper_cached(x87_id, 7), 0);
    cpu.jit_direct.set_iteration_upper_cached(x87_id, 7, 12_345);
    cpu.jit_direct.set_iteration_upper_cached(int_id, 7, 67_890);
    assert_eq!(cpu.jit_direct.iteration_upper_cached(x87_id, 7), 12_345);
    assert_eq!(cpu.jit_direct.iteration_upper_cached(int_id, 7), 67_890);

    // A different bus epoch invalidates without any CPU involvement.
    assert_eq!(cpu.jit_direct.iteration_upper_cached(x87_id, 8), 0);
    assert_eq!(cpu.jit_direct.iteration_upper_cached(int_id, 8), 0);

    // Storing under the new epoch drops the old epoch's entries rather than leaving one live.
    cpu.jit_direct.set_iteration_upper_cached(x87_id, 8, 999);
    assert_eq!(cpu.jit_direct.iteration_upper_cached(int_id, 8), 0);

    // And a mode change drops the table along with the blocks it described.
    cpu.set_mode(GswMode::Gsw486);
    assert_eq!(
        cpu.jit_direct.iteration_upper_cached(x87_id, 8),
        0,
        "a mode change must drop the per-block iteration bound memo"
    );

    // The persona timing pair is the other half of the key, and it is NOT separately testable
    // here: `key_for` admits the Direct backend on I486 and I586 only, and `level_timing` returns
    // the same (1, 12) for both, so no admissible persona pair prices a block differently. The
    // epoch covers it regardless -- the persona cannot move without `CpuGsw::set_mode`, which
    // reaches the `clear()` asserted above.
    let (mut on_486, bus_486, x87_486, _) = install_x87_and_integer_blocks(GswMode::Gsw486);
    let fresh_486 = on_486.recompute_iteration_upper_for_test(&bus_486, &x87_486);
    for _ in 0..2 {
        assert_eq!(
            on_486.iteration_upper_for_test(&bus_486, &x87_486),
            fresh_486
        );
    }
}

/// `JmpMem`'s source, read from the mode-13 aperture, must LOWER and match the interpreter
/// exactly, aggregate bus clocks included. This is the corrected shape from the review outcome:
/// `JmpMem` uses the Ret arm's construction (`emit_ram_read_pointer_inner` plus the mode13
/// completion), so unlike `PushMem`'s RAM-only source lane, a jump target read from the aperture
/// LOWERS rather than side-exiting.
///
/// This also exercises the re-load bug class Ret shipped once: `emit_mode13_read_completion`
/// clobbers RDX on its mode13 branch, and the emitter re-loads the target from RDI afterward, the
/// same fix Ret needed at `emit.rs:1028-1035`. Deleting that re-load leaves EIP at whatever the
/// clobbered increment left behind rather than the popped target.
///
/// Modelled on `a_push_through_memory_whose_source_is_the_mode13_aperture_side_exits`
/// (`cpu_jit_direct_test.rs`) rather than the RET mode13 fixtures in this file: those install a
/// block manually and step the interpreter through a fixed instruction count, and their own
/// comment records that the two accounting boundaries do not line up for a bus-clock comparison
/// ("a COMPLETED block and three interpreter cycles do not present the same accounting
/// boundary"). Driving both sides through the SAME `run_straight_line` boundary, as the PushMem
/// aperture fixture does, is what makes the aggregate bus-clock comparison meaningful here.
#[test]
fn a_jmp_through_memory_whose_source_is_the_mode13_aperture_lowers_and_matches_the_interpreter() {
    let mut memory = vec![0; 0x000b_1000];
    memory[0x100..0x108].copy_from_slice(&[
        0x90, // starter
        0x40, // inc eax
        0xff, 0x25, 0x00, 0x00, 0x0a, 0x00, // jmp dword [0xa0000], MID-BLOCK, TERMINAL
    ]);
    // The target: two more instructions, too short to compile on its own, then HLT.
    memory[0x200..0x203].copy_from_slice(&[
        0x43, // inc ebx
        0x46, // inc esi
        0xf4, // hlt
    ]);
    memory[0x000a_0000..0x000a_0004].copy_from_slice(&0x0000_0200u32.to_le_bytes());

    let mut interp = fresh();
    let mut native = fresh();
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut native_bus = TestBus::with_memory(memory);
    interp_bus.direct_pages_enabled = true;
    native_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    native_bus.direct_page_clocks = true;

    // The mode-13 aperture sits at 0xa0000, past the 0xffff real-mode segment limit, so DS must be
    // widened or the read faults instead of exercising the aperture path this fixture is about.
    for cpu in [&mut interp, &mut native] {
        make_data_segments_flat(cpu);
    }
    drive(&mut interp, &mut interp_bus);
    drive(&mut native, &mut native_bus);
    native.set_jit_auto_admit(true);
    for _ in 0..3 {
        native.halted = false;
        native.registers.eip = 0x100;
        drive(&mut native, &mut native_bus);
    }
    assert_eq!(
        native.jit_direct.len(),
        1,
        "only the source block should have compiled: the target is two slots, below the minimum"
    );

    for cpu in [&mut interp, &mut native] {
        cpu.halted = false;
        cpu.registers.eip = 0x100;
        cpu.registers.gpr.fill(0);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    for bus in [&mut interp_bus, &mut native_bus] {
        bus.trace = BusTrace::default();
    }
    let direct_before = native.perf_counters().jit_direct_insns;

    let interp_outcomes = drive(&mut interp, &mut interp_bus);
    let native_outcomes = drive(&mut native, &mut native_bus);

    assert_eq!(native_outcomes, interp_outcomes);
    assert_eq!(
        crate::tests::settled_state(&native),
        crate::tests::settled_state(&interp)
    );
    assert_eq!(native_bus.memory, interp_bus.memory);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.registers.eip, interp.registers.eip);
    // Anti-vacuity. The compiled block is the filler plus the jump (the starter is a single cold
    // visit, never part of the compiled span; see the RAM mid-block fixture for the same shape).
    // Two native instructions means the jump itself retired natively, through the mode13
    // completion, rather than the block silently stopping before it.
    assert_eq!(native.perf_counters().jit_direct_insns - direct_before, 2);
}

/// The CS-limit guard on the dynamic target: a source dword ABOVE the code segment limit must
/// side exit before EIP is ever written, and the interpreter's own re-run of the same instruction
/// must agree, because `JmpMem`'s own interpreter arm performs no such check at all
/// (`execute_extended.rs:920-924` just masks and stores). The fault comes from the FOLLOWING
/// fetch, not from the jump.
///
/// Modelled on `finite_cs_ret_limit_exit_case`, with one structural difference `Ret` never has to
/// deal with: RET's own interpreter arm checks the limit inline and faults immediately on
/// `execute_decoded`, so that fixture reproduces the fault with one call. `JmpMem`'s arm has no
/// such check, so the interpreter happily sets EIP to the too-large target and the fault only
/// appears on the NEXT fetch attempt, which this fixture reproduces as a second, explicit step.
#[test]
fn finite_cs_jmp_through_memory_limit_exit_preserves_restart_state_and_faults_precisely() {
    const ENTRY: u32 = 0x301;
    const JMP: u32 = ENTRY + 7;
    let mut memory = vec![0; 0x000b_0000];
    high_segment_page_tables(&mut memory);
    memory[0x4008..0x400c].copy_from_slice(&0x0000_7067u32.to_le_bytes());
    memory[0x8301..0x830e].copy_from_slice(&[
        0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,0x11223344
        0x89, 0xc1, // mov ecx,eax
        0xff, 0x25, 0x00, 0x20, 0x00, 0x00, // jmp dword [0x2000]
    ]);
    memory[0x7000..0x7004].copy_from_slice(&(QUAKE_CS_LIMIT + 1).to_le_bytes());

    let mut native = quake_segment_cpu(ENTRY, true);
    let mut interp = quake_segment_cpu(ENTRY, true);
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, ENTRY + 5, JMP];
    decode_segmented_fixture(&mut native, &mut native_bus, &starts);
    decode_segmented_fixture(&mut interp, &mut interp_bus, &starts);
    map_direct_page(
        &mut native,
        &mut native_bus,
        QUAKE_SEGMENT_BASE + 0x2000,
        0x7000,
        jit::fast_map::PagePermissions {
            writable: true,
            user: true,
        },
        true,
        false,
    );
    let block = install_fixture_block(&mut native, QUAKE_SEGMENT_BASE + ENTRY);
    assert_eq!(block.span().instructions, 3);
    arm_stack_fixture(&mut native, ENTRY, 0);
    arm_stack_fixture(&mut interp, ENTRY, 0);
    let side_exits = native.perf_counters().jit_direct_side_exits;
    // The CS-limit refusal now names itself instead of landing in the `Other`
    // catch-all; `Other` has no Direct producer left at all.
    let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
    let insns_before = native.perf_counters().jit_direct_insns;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(
        native.registers.eip, JMP,
        "the side exit must leave EIP at the jump itself: JmpMem writes EIP only after every \
         guard passes"
    );
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.direct_stall_snapshot().side_exit_segment_limit - limit_exits,
        1
    );
    // Anti-vacuity: only the two movs retired natively; the jump itself never completed.
    assert_eq!(native.perf_counters().jit_direct_insns - insns_before, 2);

    // The interpreter's own arm 4 has no limit check (`execute_extended.rs:920-924`): both sides
    // must execute the jump itself successfully and land EIP on the too-large target.
    let native_jmp = native
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + JMP, true)
        .unwrap();
    let interp_jmp = interp
        .decode_cache
        .get(QUAKE_SEGMENT_BASE + JMP, true)
        .unwrap();
    native
        .execute_decoded(&native_jmp, &mut native_bus)
        .unwrap();
    interp
        .execute_decoded(&interp_jmp, &mut interp_bus)
        .unwrap();
    assert_eq!(native.registers.eip, QUAKE_CS_LIMIT + 1);
    assert_eq!(interp.registers.eip, QUAKE_CS_LIMIT + 1);

    // The fault surfaces on the FOLLOWING fetch, exactly as `decode.rs`'s live fetch-limit
    // recheck documents: "enforces the fault at exactly the byte the fetch would have crossed."
    let native_fault =
        native.fetch_decoded(&mut native_bus, QUAKE_SEGMENT_BASE + native.registers.eip);
    let interp_fault =
        interp.fetch_decoded(&mut interp_bus, QUAKE_SEGMENT_BASE + interp.registers.eip);
    for fault in [native_fault, interp_fault] {
        assert!(matches!(
            fault,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            })
        ));
    }
    assert_eq!(
        crate::tests::settled_registers(&native),
        crate::tests::settled_registers(&interp)
    );
    assert_eq!(native_bus.memory, interp_bus.memory);
}

/// The slice 39 m64 aperture fixture, modelled directly on
/// `int_binary_memory_matches_the_interpreter_in_ram_and_in_the_aperture` above: drive-based,
/// `direct_page_clocks` on, comparing the aggregate bus clocks rather than a per-access trace.
///
/// This is B4's fixture: `TestBus::direct_page_wait_states` and `TestBus::mode13_wait_states`
/// already price RAM and the aperture DIFFERENTLY per width (Dword: 3 wait states in RAM, 7 in
/// the aperture), so the two dynamic dword transactions an m64 access costs are NOT
/// cost-interchangeable between the RAM and mode13 lanes here. Had the two lanes priced equally,
/// moving one of the two dynamic increments from the read lane to the wrong one (mutation 1 in
/// the design's battery) would be invisible in the aggregate; because they differ, it is not.
///
/// `esi`-relative addressing (mod=10, rm=110), same shape as the 0xDA fixture, so a
/// `read_segment` that wrongly dropped DS from the block's mask would be caught the same way.
fn fld_m64_case(target: u32) -> Vec<u8> {
    let mut code = vec![0xddu8, 0x86]; // fld qword [esi+disp32]
    code.extend_from_slice(&target.to_le_bytes());
    code.push(0x8b);
    code.push(0x96); // mov edx,[esi+disp32]
    code.extend_from_slice(&(target + 8).to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    code
}

#[test]
fn fld_m64_matches_the_interpreter_in_ram_and_in_the_aperture() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    for target in [RAM, MODE13] {
        let code = fld_m64_case(target);
        let mut memory = vec![0; 0x000b_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[target as usize..target as usize + 8]
            .copy_from_slice(&12.5f64.to_bits().to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        native.registers.eip = ENTRY;
        interp.registers.eip = ENTRY;
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 6, ENTRY + 12];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                target,
                target,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                false,
            );
            cpu.fpu = X87::default();
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            cpu.set_eip(ENTRY);
            cpu.registers.set_esi(0);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 3;
            cpu.core_clocks_so_far = 0;
            bus.trace = BusTrace::default();
        }
        let label = format!("fld m64 at {target:#x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: did not run directly"
        );
        for _ in 0..block.span().instructions {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
        assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{label}: bus clocks"
        );
        assert_eq!(native.fpu.get(0), 12.5, "{label}: FLD result");
    }
}

/// The slice 39 mutation battery's coverage gap, closed: mutation 2 (B2's re-aimed catcher,
/// `emit_x87_memory_completion`'s RAM WRITE Qword arm incrementing by 1 instead of 2) SURVIVED
/// against `fld_fadd_fdiv_and_fstp_m64_match_the_interpreter_and_preserve_the_full_range_value`
/// (`cpu_jit_x87_direct_test.rs`), because that fixture's bus comes from `direct_memory`, which
/// never sets `direct_page_clocks`. `TestBus::jit_data_cost_clocks` returns 0 whenever that flag
/// is clear, so the RAM dword-write lane's bus price is invisible there regardless of
/// `ram_dword_writes`'s value: the differential proves STATE and STORE correctness, not the RAM
/// write bus charge.
///
/// This fixture prices it for real, modelled on `fld_m64_matches_the_interpreter_in_ram_and_in_the_aperture`
/// above with `direct_page_clocks` on: FLD1 (no memory access) then FSTP m64 to `esi`-relative
/// memory, comparing aggregate bus clocks against the interpreter's two independent dword writes
/// (`write_qword`, `fpu_exec.rs:742-764`). Distinct RAM and mode13 wait states (B4's requirement)
/// make a missing dword-write charge visible in either lane.
fn fstp_m64_case(target: u32) -> Vec<u8> {
    let mut code = vec![0xd9u8, 0xe8]; // fld1                    ST(0) = 1.0
    code.push(0xdd);
    code.push(0x9e); // fstp qword [esi+disp32]  /3
    code.extend_from_slice(&target.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    code
}

#[test]
fn fstp_m64_matches_the_interpreter_in_ram_and_in_the_aperture() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    for target in [RAM, MODE13] {
        let code = fstp_m64_case(target);
        let mut memory = vec![0; 0x000b_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        native.registers.eip = ENTRY;
        interp.registers.eip = ENTRY;
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 2, ENTRY + 8];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                target,
                target,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                true,
            );
            cpu.fpu = X87::default();
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            cpu.set_eip(ENTRY);
            cpu.registers.set_esi(0);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 3;
            cpu.core_clocks_so_far = 0;
            bus.trace = BusTrace::default();
        }
        let label = format!("fstp m64 at {target:#x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: did not run directly"
        );
        for _ in 0..block.span().instructions {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
        assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{label}: bus clocks"
        );
        assert_eq!(
            f64::from_bits(u64::from_le_bytes(
                native_bus.memory[target as usize..target as usize + 8]
                    .try_into()
                    .unwrap()
            )),
            1.0,
            "{label}: FSTP result"
        );
    }
}

/// Slice 40's fixture 5, the aperture timing fixture: FILD m64 in RAM and in the mode-13 window,
/// modelled directly on `fld_m64_matches_the_interpreter_in_ram_and_in_the_aperture` above, with
/// two changes. First, `direct_page_wait_states`/`mode13_wait_states` price Dword differently per
/// lane (3 wait states in RAM, 7 in the aperture), so the two dynamic dword-read transactions an
/// m64 access costs are NOT cost-interchangeable between the lanes, the same B4 argument that
/// fixture applies. Second, and new to this slice, an `inc eax` precedes the FILD so it is NOT
/// the block's first instruction: the review outcome requires every slice 40 fixture to be
/// strictly mid-block, closing the entry-position gap slice 39's own aperture fixtures left open.
fn fild_m64_aperture_case(target: u32) -> Vec<u8> {
    let mut code = vec![0x40u8]; // inc eax, keeps the FILD off the block ENTRY
    code.push(0xdf);
    code.push(0xae); // fild qword [esi+disp32]
    code.extend_from_slice(&target.to_le_bytes());
    code.push(0x8b);
    code.push(0x96); // mov edx,[esi+disp32]
    code.extend_from_slice(&(target + 8).to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]); // mov esi,esi ; hlt
    code
}

#[test]
fn fild_m64_matches_the_interpreter_in_ram_and_in_the_aperture() {
    const ENTRY: u32 = 0x101;
    const RAM: u32 = 0x0003_0000;
    const MODE13: u32 = 0x000a_0000;
    const VALUE: i64 = 123_456_789_012_345;
    for target in [RAM, MODE13] {
        let code = fild_m64_aperture_case(target);
        let mut memory = vec![0; 0x000b_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[target as usize..target as usize + 8].copy_from_slice(&VALUE.to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        native.registers.eip = ENTRY;
        interp.registers.eip = ENTRY;
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 13];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                target,
                target,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                false,
            );
            cpu.fpu = X87::default();
        }
        let block = install_fixture_block(&mut native, ENTRY);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            cpu.set_eip(ENTRY);
            cpu.registers.set_esi(0);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 3;
            cpu.core_clocks_so_far = 0;
            bus.trace = BusTrace::default();
        }
        let label = format!("fild m64 at {target:#x}");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: did not run directly"
        );
        for _ in 0..block.span().instructions {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
        assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{label}: bus clocks"
        );
        assert_eq!(native.fpu.get(0), VALUE as f64, "{label}: FILD result");
    }
}

/// Slice 40's fixture 3, the mid-block value differential: FILD m64 on two magnitudes the i32
/// convert primitive cannot represent correctly, DS-based (plain disp32 addressing, no ESI and
/// no segment override) and `direct_page_clocks` on so the aggregate bus clocks price the two
/// dynamic dword reads for real, not just the state. An `inc eax` precedes the FILD so it is
/// never the block's first instruction, and `block.span().instructions` is pinned to a literal
/// (not merely compared to itself) so a classify regression that dropped FILD from the block
/// would shrink the span below the pin rather than passing silently -- the insns-delta gate the
/// review outcome calls for.
///
/// The first value, 5_000_000_000i64, is just above 2^32 (~4.29e9): a value that fit in 32 bits
/// would still convert correctly through the WRONG i32 primitive (mutation 1 in the design's
/// battery) and make this fixture vacuous. The second, `(1i64 << 54) + 1`, is above 2^53, f64's
/// mantissa limit, so the conversion genuinely ROUNDS (B5): both sides must still agree, because
/// they share the host's default MXCSR.RC = 00 (round-to-nearest-even), which is also what
/// Rust's `as f64` cast (used both here and by the interpreter) uses.
fn fild_m64_disp32_case(target: u32) -> Vec<u8> {
    let mut code = vec![0x40u8]; // inc eax, keeps the FILD off the block ENTRY
    code.push(0xdf);
    code.push(0x2d); // fild qword [disp32], mod=00 reg=5 rm=101 (disp32-only, DS-based)
    code.extend_from_slice(&target.to_le_bytes());
    code.push(0x43); // inc ebx
    code.push(0xf4); // hlt
    code
}

#[test]
fn fild_m64_above_2_32_and_2_53_matches_the_interpreter_mid_block() {
    const ENTRY: u32 = 0x101;
    const TARGET: u32 = 0x0003_0000;
    const EXPECTED_INSTRUCTIONS: u8 = 3; // inc eax, fild, inc ebx (HLT terminates, not counted)
    for value in [5_000_000_000i64, (1i64 << 54) + 1] {
        let code = fild_m64_disp32_case(TARGET);
        let mut memory = vec![0; 0x0004_0000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        memory[TARGET as usize..TARGET as usize + 8].copy_from_slice(&value.to_le_bytes());

        let mut native = fresh();
        let mut interp = fresh();
        make_data_segments_flat(&mut native);
        make_data_segments_flat(&mut interp);
        native.registers.eip = ENTRY;
        interp.registers.eip = ENTRY;
        let mut native_bus = TestBus::with_memory(memory.clone());
        let mut interp_bus = TestBus::with_memory(memory);
        for bus in [&mut native_bus, &mut interp_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
            bus.report_batch_clocks = true;
            bus.uniform_native_fetches = true;
        }
        let starts = [ENTRY, ENTRY + 1, ENTRY + 7, ENTRY + 8];
        decode_fixture(&mut native, &mut native_bus, &starts);
        decode_fixture(&mut interp, &mut interp_bus, &starts);
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            map_direct_page(
                cpu,
                bus,
                TARGET,
                TARGET,
                jit::fast_map::PagePermissions::UNPAGED,
                true,
                false,
            );
            cpu.fpu = X87::default();
        }
        let block = install_fixture_block(&mut native, ENTRY);
        assert_eq!(
            block.span().instructions,
            EXPECTED_INSTRUCTIONS,
            "value {value}: FILD must join the native block rather than fall to the interpreter"
        );
        for (cpu, bus) in [
            (&mut native, &mut native_bus),
            (&mut interp, &mut interp_bus),
        ] {
            cpu.set_eip(ENTRY);
            cpu.elapsed_clocks = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 3;
            cpu.core_clocks_so_far = 0;
            bus.trace = BusTrace::default();
        }
        let label = format!("fild m64 value {value}");
        let retired_before = native.perf_counters().jit_direct_insns;
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: did not run directly"
        );
        assert_eq!(
            native.perf_counters().jit_direct_insns - retired_before,
            u64::from(EXPECTED_INSTRUCTIONS),
            "{label}: not every instruction in the block retired natively"
        );
        for _ in 0..block.span().instructions {
            interp.cycle(&mut interp_bus).unwrap();
        }

        assert_eq!(native.fpu, interp.fpu, "{label}: x87 state");
        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interp),
            "{label}: registers"
        );
        assert_eq!(native_bus.memory, interp_bus.memory, "{label}: memory");
        assert_eq!(
            native.elapsed_clocks, interp.elapsed_clocks,
            "{label}: core clocks"
        );
        assert_eq!(native.fp_rem, interp.fp_rem, "{label}: x87 remainder");
        assert_eq!(
            native_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "{label}: bus clocks"
        );
        assert_eq!(native.fpu.get(0), value as f64, "{label}: FILD result");
    }
}
