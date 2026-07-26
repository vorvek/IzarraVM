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
    let start_pending = native.pending_flags;
    let zero_budget_rejects = native.perf_counters().jit_direct_reject_zero_budget;

    assert!(
        !native
            .try_run_direct_block_with_cap_for_test(&mut native_bus, block, iteration_upper)
            .unwrap()
    );
    assert_eq!(native.registers, start_registers);
    assert_eq!(native.pending_flags, start_pending);
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

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
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

                assert_eq!(native.registers, interp.registers, "{label}: registers");
                assert_eq!(native.pending_flags, interp.pending_flags, "{label}: flags");
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

#[test]
fn movzx_register_and_word_operand_forms_remain_interpreter_only() {
    const ENTRY: u32 = 0x101;
    for code in [
        vec![0x0fu8, 0xb6, 0xd8], // MOVZX ebx, al: REGISTER form, not lowered
        vec![0x0f, 0xb7, 0xd8],   // MOVZX ebx, ax
        vec![0x0f, 0xbe, 0xd8],   // MOVSX ebx, al
        vec![0x0f, 0xbf, 0xd8],   // MOVSX ebx, ax
        // 66-prefixed memory forms. The OperandSize::Word gate is the only thing stopping these,
        // and write_gpr_sized at Word MERGES into the low 16 bits instead of replacing all 32, so
        // lowering one as the 32-bit form would clobber the destination's high half.
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
        assert!(
            jit::direct::compile(&mut cpu, ENTRY, true).is_none(),
            "{code:02x?} must stay interpreter-only"
        );
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
        block.data_descriptors_match(&cpu),
        "the block must match the segments it was compiled under"
    );
    let mut reloaded = cpu.registers.segment(SegmentIndex::Ds);
    reloaded.base = 0x1_0000;
    cpu.registers.set_segment(SegmentIndex::Ds, reloaded);
    assert!(
        !block.data_descriptors_match(&cpu),
        "reloading DS must invalidate a block that reads through DS"
    );
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

            assert_eq!(native.registers, interp.registers, "{label}: registers");
            assert_eq!(
                native.pending_flags, interp.pending_flags,
                "{label}: pending"
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
    assert_eq!(native.registers, interp.registers, "aliased dst/base");
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
        assert_eq!(native.registers, interp.registers, "{label}: registers");
        // The raw descriptor, not just eflags(): a tail that materialized eagerly, or that skipped
        // emit_clear_pending, agrees on eflags() while differing on every byte of this.
        assert_eq!(
            native.pending_flags, interp.pending_flags,
            "{label}: lazy flags"
        );
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
    code.extend_from_slice(&[0x66, 0x87, 0xc0]);
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
        direct.registers, interpreter.registers,
        "{} {mode:?}",
        case.name
    );
    assert_eq!(
        direct.pending_flags, interpreter.pending_flags,
        "{} {mode:?} pending flags",
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
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
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

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
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

    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.pending_flags, interp.pending_flags);
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

        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.pending_flags, interp.pending_flags);
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
    let other_exits = native.perf_counters().jit_direct_exit_other;

    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    interp.cycle(&mut interp_bus).unwrap();
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(native.registers, interp.registers);
    assert_eq!(native.registers.eip, RET);
    assert_eq!(native.registers.esp(), INITIAL_ESP);
    assert_eq!(native.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        native_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
    assert_eq!(native.perf_counters().jit_direct_side_exits - side_exits, 1);
    assert_eq!(
        native.perf_counters().jit_direct_exit_other - other_exits,
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
    assert_eq!(native.registers, interp.registers);
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
        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.registers.eip, STORE);
        assert_eq!(native.pending_flags, interp.pending_flags);
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
        assert_eq!(native.registers, interp.registers);
        assert_eq!(native.pending_flags, interp.pending_flags);
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
    assert!(cpu.jit_direct.has_linked_successor(old_source));

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
    assert!(!cpu.jit_direct.has_linked_successor(new_source));

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
    assert!(cpu.jit_direct.has_linked_successor(new_source));

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

#[test]
fn word_renderer_slice_is_admitted_only_for_586() {
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

    for (mode, expected_instructions) in [(GswMode::Gsw486, 3), (GswMode::Gsw586, 6)] {
        let mut memory = vec![0; 0x1000];
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut cpu = flat_stack_cpu(ENTRY);
        cpu.set_mode(mode);
        let mut bus = TestBus::with_memory(memory);
        decode_fixture(&mut cpu, &mut bus, &starts);

        let block = install_fixture_block(&mut cpu, ENTRY);
        assert_eq!(block.span().instructions, expected_instructions, "{mode:?}");
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

    assert_eq!(direct.registers, interpreter.registers);
    assert_eq!(direct.pending_flags, interpreter.pending_flags);
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
