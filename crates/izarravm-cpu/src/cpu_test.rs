// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_bus::{BusCycle, BusTrace, BusWidth, DirectPage};
use izarravm_bus::{DirectMemoryRead, DirectMemoryWrite};

/// The JIT's emitted native code addresses `gpr[i]` as `[regs_ptr + 4*i]`, relying on
/// `Registers` being `repr(C)` with `gpr` as the first field. A rustc or field reorder that broke
/// this offset would silently corrupt guest state through wrong native loads/stores, so this
/// test freezes the layout assumption the JIT bakes into its emitted bytes. The eip offset is
/// asserted too (the dispatch reads it); eflags follows eip at +4.
#[test]
fn registers_repr_c_offsets_are_stable() {
    // gpr is the first field of repr(C) Registers: offset 0, 4-byte element stride.
    assert_eq!(core::mem::offset_of!(Registers, gpr), 0);
    assert_eq!(core::mem::size_of::<u32>(), 4);
    // eip sits after gpr (32 bytes) + segments ([SegmentRegister; 6]). repr(C) guarantees this
    // declaration order is the memory order; eflags immediately follows eip at +4.
    let eip_off = core::mem::offset_of!(Registers, eip);
    assert_eq!(eip_off, 32 + core::mem::size_of::<[SegmentRegister; 6]>());
    assert_eq!(core::mem::offset_of!(Registers, eflags), eip_off + 4);
}

/// The paged JIT probe bakes this entry stride and these field offsets into native code.
#[test]
#[cfg(feature = "jit")]
fn tlb_entry_repr_c_offsets_are_stable() {
    assert_eq!(core::mem::size_of::<TlbEntry>(), 16);
    assert_eq!(core::mem::offset_of!(TlbEntry, tag), 0);
    assert_eq!(core::mem::offset_of!(TlbEntry, phys), 4);
    assert_eq!(core::mem::offset_of!(TlbEntry, generation), 8);
    assert_eq!(core::mem::offset_of!(TlbEntry, writable), 12);
    assert_eq!(core::mem::offset_of!(TlbEntry, user), 13);
    assert_eq!(core::mem::offset_of!(TlbEntry, dirty), 14);
}

/// The v2 region emitter bakes `offset_of!(CpuGsw, registers)` into its emitted bytes (the
/// prologue computes `regs_ptr = cpu_ptr + regs_offset`, and inline slots address gpr as
/// `[regs_ptr + 4*i]`). CpuGsw is NOT repr(C), so rustc is free to reorder its fields; this
/// test pins the current offset so a rustc version bump that moved `registers` is caught here
/// (the emitter reads the offset at emit time, so a changed value still produces correct code,
/// but the assertion documents the layout and guards against a silent perf shift from a
/// changed cache-line placement of gpr).
#[test]
#[cfg(feature = "jit")]
fn cpu_registers_field_offset_is_stable() {
    let off = core::mem::offset_of!(CpuGsw, registers);
    // The current layout places `registers` at a non-zero offset (rustc reorders CpuGsw's
    // fields for alignment). The emitter handles any value (it bakes `offset_of!` at emit
    // time, verified by the differential suites jit_region + jit_general); this assertion
    // freezes the known position so a change is visible. The constant tracks the live layout
    // (456 -> 464 when Round 1 added the `jit_table_clears` u64 to PerfCounters, which precedes
    // `registers`; the emitter re-reads the offset, so this is a documentation update).
    assert_eq!(
        off, 472,
        "CpuGsw.registers offset moved; update the emitter's baked offset"
    );
}

#[test]
fn perf_counter_tracks_code_invalidation_events() {
    let mut cpu = CpuGsw::default();
    let before = cpu.perf_counters().code_invalidations;

    cpu.note_a20_changed();

    assert_eq!(cpu.perf_counters().code_invalidations, before + 1);
}

#[test]
fn structural_code_invalidation_clears_stale_native_watch_marks() {
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0x100;
    let mut memory = vec![0; 0x1000];
    memory[0x100] = 0x90;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    let page = 0x100u32 >> 12;
    assert!(cpu.decode_cache.range_hits_code(0x100, 1));
    assert_ne!(
        cpu.decode_cache.code_pages[(page >> 6) as usize] & (1u64 << (page & 63)),
        0
    );
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(cpu.decode_cache.native_code_watch.is_watched(0x100));

    cpu.note_a20_changed();

    assert!(!cpu.decode_cache.range_hits_code(0x100, 1));
    assert_eq!(
        cpu.decode_cache.code_pages[(page >> 6) as usize] & (1u64 << (page & 63)),
        0
    );
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    assert!(!cpu.decode_cache.native_code_watch.is_watched(0x100));
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn interpreter_direct_pages_skip_fast_map_when_native_admission_is_disabled() {
    let mut bus = TestBus::with_memory(vec![0; 0x6000]);
    bus.direct_pages_enabled = true;
    bus.memory[0x2456] = 0x3c;
    bus.memory[0x3456] = 0x5a;
    let mut cpu = CpuGsw::default();

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x2456, BusAccessKind::DataRead,)
            .unwrap(),
        0x3c
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x2456, 0x2456));
    cpu.set_jit_auto_admit(true);

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0x5a
    );
    assert!(cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));
    cpu.set_jit_auto_admit(false);
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        0x3456,
        0xa5,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));
    assert_eq!(bus.memory[0x3456], 0xa5);

    cpu.note_direct_map_changed();
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert!(!cpu.jit_fast_map.has_write_mapping(0x3456, 0x3456));

    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0xa5
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));

    cpu.flush_tlb_and_code_caches();
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
    assert_eq!(
        cpu.read_memory_u8(&mut bus, SegmentIndex::Ds, 0x3456, BusAccessKind::DataRead,)
            .unwrap(),
        0xa5
    );
    assert!(!cpu.jit_fast_map.has_read_mapping(0x3456, 0x3456));
}

#[cfg(feature = "jit")]
#[test]
fn direct_admission_heat_is_per_cpu_instance() {
    let mut changed = CpuGsw::default();
    let unchanged = CpuGsw::default();

    changed.jit_direct.set_admission_heat_for_test(8);

    assert_eq!(changed.jit_direct.admission_heat(), 8);
    assert_eq!(unchanged.jit_direct.admission_heat(), 1);
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn physical_page_cache_hits_fill_every_paging_alias() {
    const ALIAS_A: u32 = 0x3000;
    const ALIAS_B: u32 = 0x4000;
    const FRAME: u32 = 0x5000;
    const OFFSET: u32 = 0x120;

    let mut memory = vec![0; 0x7000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[0x2010..0x2014].copy_from_slice(&0x0000_5007u32.to_le_bytes());
    memory[(FRAME + OFFSET) as usize..(FRAME + OFFSET + 4) as usize]
        .copy_from_slice(&0x4433_2211u32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.set_jit_auto_admit(true);

    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A + OFFSET,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x11
    );
    let page_fills = cpu.perf_counters().direct_page_hits;
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_B + OFFSET,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x11
    );
    assert_eq!(cpu.perf_counters().direct_page_hits, page_fills);
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    assert!(
        !cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, 0x6000 + OFFSET)
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_A);
    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_A + OFFSET,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x4433_2211
    );
    assert_eq!(
        cpu.read_memory_sized(
            &mut bus,
            SegmentIndex::Ds,
            ALIAS_B + OFFSET,
            OperandSize::Dword,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x4433_2211
    );
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_read_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );

    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_A + OFFSET,
        0x55,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_B + OFFSET,
        0x66,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_A);
    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_A + OFFSET,
        OperandSize::Dword,
        0xaabb_ccdd,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        ALIAS_B + OFFSET,
        OperandSize::Dword,
        0x1020_3040,
        BusAccessKind::DataWrite,
    )
    .unwrap();
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    assert_eq!(
        &bus.memory[(FRAME + OFFSET) as usize..(FRAME + OFFSET + 4) as usize],
        &0x1020_3040u32.to_le_bytes()
    );

    cpu.jit_fast_map.invalidate_page(ALIAS_B);
    assert!(
        cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
    assert!(
        !cpu.jit_fast_map
            .has_write_mapping(ALIAS_B + OFFSET, FRAME + OFFSET)
    );
    cpu.note_direct_map_changed();
    assert!(
        !cpu.jit_fast_map
            .has_write_mapping(ALIAS_A + OFFSET, FRAME + OFFSET)
    );
}

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn active_fast_map_survives_small_tlb_collision_without_page_walk() {
    const LINEAR_A: u32 = 0x0000_3000;
    const LINEAR_B: u32 = LINEAR_A + TLB_ENTRIES as u32 * 0x1000;
    const FRAME_A: u32 = 0x0000_5000;
    const FRAME_B: u32 = 0x0000_6000;

    let mut memory = vec![0; 0x8000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    let pte_a = 0x2000 + ((LINEAR_A >> 12) as usize * 4);
    let pte_b = 0x2000 + ((LINEAR_B >> 12) as usize * 4);
    memory[pte_a..pte_a + 4].copy_from_slice(&(FRAME_A | 7).to_le_bytes());
    memory[pte_b..pte_b + 4].copy_from_slice(&(FRAME_B | 7).to_le_bytes());
    memory[FRAME_A as usize] = 0xa5;
    memory[FRAME_B as usize] = 0x5a;

    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    let mut cpu = CpuGsw::default();
    cpu.set_jit_auto_admit(true);
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x10, 0x93));

    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_A,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0xa5
    );
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_B,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0x5a
    );
    assert!(cpu.tlb.lookup(LINEAR_A >> 12).is_none());
    assert!(cpu.jit_fast_map.has_read_mapping(LINEAR_A, FRAME_A));

    bus.trace.clear();
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            LINEAR_A,
            BusAccessKind::DataRead,
        )
        .unwrap(),
        0xa5
    );
    assert!(bus.trace.cycles().iter().all(|cycle| !matches!(
        cycle.kind,
        BusAccessKind::PageWalkRead | BusAccessKind::PageWalkWrite
    )));
}

#[test]
#[cfg(feature = "jit")]
fn region_ctx_fn_pointer_offsets() {
    // Pin ALL offsets the emitted native code reads/writes so a field reorder is caught.
    use jit::step::RegionCtx;
    assert_eq!(core::mem::offset_of!(RegionCtx, step_fn), 0);
    assert_eq!(core::mem::offset_of!(RegionCtx, inline_step_fn), 8);
    assert_eq!(core::mem::offset_of!(RegionCtx, set_pending_add_fn), 16);
    assert_eq!(core::mem::offset_of!(RegionCtx, set_shift_flags_fn), 24);
    assert_eq!(core::mem::offset_of!(RegionCtx, native_u8_fn), 32);
    // Pending flags offset used by direct native writes.
    assert_eq!(core::mem::offset_of!(CpuGsw, pending_flags), 4280);
}

/// The JIT's `jit_set_pending_add` helper must construct the identical pending descriptor the
/// interpreter's `alu_add(a, b, 0, Dword)` does, so that a later flag read (or materialization)
/// sees the same six arithmetic bits. Swept across operand pairs that exercise the carry,
/// zero, sign, overflow, half-carry, and parity paths. The comparison goes through
/// `materialized_eflags`, the same reader the interpreter uses, so it is exact.
#[cfg(feature = "jit")]
#[test]
fn jit_set_pending_add_matches_alu_add() {
    let probes = [
        (0u32, 0u32),
        (1, 1),
        (0xffff_ffff, 1),
        (0x7fff_ffff, 1),
        (0x8000_0000, 0x8000_0000),
        (0x0f, 0x01),
        (0x1f, 0x01),
        (0x1234_5678, 0x9abc_def0),
        (0xffff_ffff, 0xffff_ffff),
        (0x0000_00ff, 0x0000_0001),
    ];
    for &(a, b) in &probes {
        let mut ref_cpu = CpuGsw::default();
        ref_cpu.alu_add(a, b, 0, BusWidth::Dword);
        let ref_ef = ref_cpu.materialized_eflags();

        let mut jit_cpu = CpuGsw::default();
        jit_cpu.jit_set_pending_add(a, b);
        let jit_ef = jit_cpu.materialized_eflags();

        assert_eq!(
            jit_ef, ref_ef,
            "jit_set_pending_add({a:#x}, {b:#x}) flags diverge from alu_add"
        );
        // The descriptor itself must match too (cf_override, op, width, result).
        assert_eq!(
            jit_cpu.pending_flags, ref_cpu.pending_flags,
            "jit_set_pending_add({a:#x}, {b:#x}) descriptor diverges"
        );
    }
}

/// The JIT's `jit_set_shift_flags_shr` helper must leave the identical flag state the
/// interpreter's `shift_rotate(5, value, count, Dword)` does, for every count 0..=31 and a set
/// of values that exercise CF (last bit out), OF (count==1 MSB), ZF/SF/PF (result), and the
/// AF/OF-preserved paths (count != 1). This is the hardest correctness property of the inline
/// SHR slots: a divergence here would corrupt the jnz back-edge decision.
#[cfg(feature = "jit")]
#[test]
fn jit_set_shift_flags_shr_matches_shift_rotate() {
    let values = [
        0u32,
        1,
        0x8000_0000,
        0xffff_ffff,
        0x7fff_ffff,
        0x4000_0000,
        0x0200_0000, // bit 25 set: CF probe for count 26
        0x0100_0000, // bit 24 set: CF probe for count 25 (the drawcolumn shift)
        0x1234_5678,
        0x0000_0001,
    ];
    for &value in &values {
        for count in 0u8..=31 {
            let mut ref_cpu = CpuGsw::default();
            // Seed a non-trivial pending descriptor first, so the slow path of
            // set_shift_result_flags (fold-then-eager) is exercised, matching the real loop
            // where an earlier add slot leaves a descriptor outstanding.
            ref_cpu.alu_add(0x1000, 0x2000, 0, BusWidth::Dword);
            ref_cpu.shift_rotate(5, value, count, BusWidth::Dword);
            let ref_ef = ref_cpu.materialized_eflags();

            let mut jit_cpu = CpuGsw::default();
            jit_cpu.alu_add(0x1000, 0x2000, 0, BusWidth::Dword);
            jit_cpu.jit_set_shift_flags_shr(value, count);
            let jit_ef = jit_cpu.materialized_eflags();

            assert_eq!(
                jit_ef, ref_ef,
                "jit_set_shift_flags_shr({value:#x}, {count}) flags diverge from shift_rotate"
            );
            // No descriptor should be outstanding after a shift (shifts materialize eagerly).
            assert_eq!(
                jit_cpu.pending_flags.tag & (1u32 << 31) != 0,
                ref_cpu.pending_flags.tag & (1u32 << 31) != 0,
                "jit_set_shift_flags_shr({value:#x}, {count}) pending-state diverges"
            );
        }
    }
}

/// Measure fully register-allocated native code against the interpreter. Runs a
/// 32-bit flat drawcolumn-shaped loop
/// (15 instructions, 7 memory ops) through the REAL interpreter (`run_straight_line`) and through
/// a hand-emitted native version that keeps every guest register in a host register and folds the
/// texture base into a host pointer so each guest base+index memory operand lowers to one host SIB
/// access (no per-access address-add, which would clobber the loop's live flags). The two runs
/// execute on identical fresh memory and their framebuffers are compared byte-for-byte, so a
/// codegen bug fails the test instead of faking a speed number. The speedup is an OPTIMISTIC
/// ceiling (best-case register allocation + raw-pointer memory vs a lean TestBus interpreter); the
/// realistic dynarec lands below it. Run:
///   cargo test -j8 -p izarravm-cpu --release --features jit g0_prime_cpu_ceiling -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_cpu_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const CODE: u32 = 0x0000;
    const TEX: u32 = 0x1000; // 512-byte texture region
    const COUNT_ADDR: u32 = 0x3000;
    const FB: u32 = 0x0010_0000; // framebuffer
    const STRIDE: u32 = 0x50; // guest edi advance per iteration (bytes)
    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 200_000;
    const TRIALS: usize = 7;
    const FB_LEN: usize = ITERS as usize * STRIDE as usize;
    const MEM_LEN: usize = FB as usize + FB_LEN + 0x1000;

    // --- guest loop bytes (32-bit); a trailing HLT ends run_straight_line ---
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP1
    code.extend_from_slice(&STEP1.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x07]); // mov [edi],eax
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP2
    code.extend_from_slice(&STEP2.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x5F, 0x04]); // mov [edi+4],ebx
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x81, 0xC7]); // add edi,STRIDE
    code.extend_from_slice(&STRIDE.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    code.extend_from_slice(&[0xFF, 0x0D]); // dec dword [COUNT_ADDR]
    code.extend_from_slice(&COUNT_ADDR.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8; // back to CODE (offset 0)
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt
    assert!(
        code.len() < TEX as usize,
        "code overruns the texture region"
    );

    let build_mem = || {
        let mut m = vec![0u8; MEM_LEN];
        m[..code.len()].copy_from_slice(&code);
        for i in 0..512u32 {
            m[(TEX + i) as usize] = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        m[COUNT_ADDR as usize..COUNT_ADDR as usize + 4].copy_from_slice(&ITERS.to_le_bytes());
        m
    };

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };
    let setup = |cpu: &mut CpuGsw| {
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b)); // 32-bit code
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93)); // data
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Es, seg(0x10, 0x93));
        cpu.registers.eip = CODE;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(TEX);
        cpu.registers.set_edi(FB);
    };

    // --- native emission: guest regs pinned in host regs, memory via host pointers ---
    // ebp=R8 ecx=R9 edx=R10 eax=R11 ebx=RBX ; esi_host=R12 (ram+TEX) edi_host=R13 (ram+FB) count=R14
    // arg0(RCX)=ram_base, arg1(RDX)=iters.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.mov_r64_r64(Reg::R12, Reg::RCX);
        e.add_r64_imm32(Reg::R12, TEX); // esi_host = ram_base + TEX
        e.mov_r64_r64(Reg::R13, Reg::RCX);
        e.add_r64_imm32(Reg::R13, FB); // edi_host = ram_base + FB
        e.mov_r32_r32(Reg::R14, Reg::RDX); // count = iters
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1); // add ebp,STEP1
        e.store_r32_disp8(Reg::R13, 0, Reg::R11); // mov [edi],eax
        e.shr_r32_imm8(Reg::R9, 24); // shr ecx,24
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2); // add ebp,STEP2
        e.store_r32_disp8(Reg::R13, 4, Reg::RBX); // mov [edi+4],ebx
        e.shr_r32_imm8(Reg::R10, 24); // shr edx,24
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx]
        e.add_r64_imm32(Reg::R13, STRIDE); // add edi,STRIDE (host ptr)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx]
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec count (sets ZF)
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx] (no flag change)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx] (no flag change)
        e.jnz(top); // jnz entry
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed on the dev host")
    };
    type NativeFn = unsafe extern "C" fn(*mut u8, u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        // interpreter: run_straight_line chains a bounded number of instructions per call then
        // returns (a non-continuable insn / the final HLT ends the run), exactly as under the
        // machine. Drive it until the guest loop counter reaches 0.
        let mut cpu = CpuGsw::default();
        setup(&mut cpu);
        let mut bus = TestBus::with_memory(build_mem());
        // TestBus defaults to Full tracing (an unbounded per-access cycle Vec) — a test
        // instrumentation cost the real MachineBus never pays. Disable it so the interpreter
        // baseline is representative. (Residual caveat: TestBus still lacks MachineBus's cached
        // raw-pointer direct-page path, so it is marginally slower than the real bus — a small
        // bias in the native side's favor, noted in the results.)
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let count_of = |bus: &TestBus| {
            u32::from_le_bytes(
                bus.memory[COUNT_ADDR as usize..COUNT_ADDR as usize + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || count_of(&bus) == 0 {
                break;
            }
            assert!(
                calls < ITERS as u64 * 4 + 1000,
                "run_straight_line not converging (calls={calls}, left={})",
                count_of(&bus)
            );
        }
        let ins = t.elapsed().as_secs_f64();
        let left = count_of(&bus);
        assert_eq!(
            left, 0,
            "loop did not run all {ITERS} iterations (count={left})"
        );
        if trial == 0 {
            eprintln!(
                "(interp chaining: {calls} run_straight_line calls for {ITERS} iters = {:.1} insns/call)",
                insns as f64 / calls as f64
            );
        }

        // native (identical fresh memory)
        let mut memn = build_mem();
        let t = Instant::now();
        unsafe { native_fn(memn.as_mut_ptr(), ITERS) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: framebuffers must be byte-identical
        let a = &bus.memory[FB as usize..FB as usize + FB_LEN];
        let b = &memn[FB as usize..FB as usize + FB_LEN];
        if a != b {
            let idx = a.iter().zip(b).position(|(x, y)| x != y).unwrap();
            panic!(
                "native framebuffer diverges from interpreter at FB+{idx}: interp={} native={}",
                a[idx], b[idx]
            );
        }

        if trial > 0 {
            // discard trial 0 (cold host caches)
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== CPU ceiling probe ({ITERS} iters x 15 insns, median of {TRIALS}) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native (best-case, reg-allocated + raw-ptr mem) : {mn:.3} ns/guest-insn");
    eprintln!(
        "SPEEDUP CEILING : {:.2}x   [4x = 'already very good' bar]",
        mi / mn
    );
    eprintln!("=== end memory-mixed probe ===\n");
}

/// Dispatch-only companion to the CPU ceiling probe. A 15-instruction register-only loop
/// (no memory operands at all, so ZERO bus involvement — the TestBus memory-path caveat cannot
/// apply) isolating the interpreter's pure per-instruction dispatch/decode/flag/clock-accounting
/// overhead vs native register ops. This anchors that the memory-mixed interpreter figure is real
/// and not a bus artifact. Correctness is checked by comparing final guest register values.
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_dispatch_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 300_000;
    const TRIALS: usize = 7;

    // register-only guest loop (counter in edi); trailing HLT.
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add ebp,STEP1
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebp,STEP2
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0xC1]); // mov eax,ecx
    code.extend_from_slice(&[0x81, 0xC0]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add eax,STEP1
    code.extend_from_slice(&[0xC1, 0xE8, 0x03]); // shr eax,3
    code.extend_from_slice(&[0x8B, 0xDA]); // mov ebx,edx
    code.extend_from_slice(&[0x81, 0xC3]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebx,STEP2
    code.extend_from_slice(&[0xC1, 0xEB, 0x03]); // shr ebx,3
    code.extend_from_slice(&[0x81, 0xC6]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add esi,STEP1
    code.extend_from_slice(&[0x81, 0xC7]);
    code.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // add edi,-1 (dec, sets ZF)
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8;
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };

    // native register-only emission; final regs written to out[0..6] = eax,ebx,ecx,edx,ebp,esi.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r32_r32(Reg::R14, Reg::RCX); // count = iters (arg0)
        e.mov_r64_r64(Reg::R15, Reg::RDX); // out ptr (arg1)
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        e.mov_r32_imm32(Reg::R12, 0); // esi
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1);
        e.shr_r32_imm8(Reg::R9, 24);
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2);
        e.shr_r32_imm8(Reg::R10, 24);
        e.mov_r32_r32(Reg::R11, Reg::R9); // mov eax,ecx
        e.add_r32_imm32(Reg::R11, STEP1);
        e.shr_r32_imm8(Reg::R11, 3);
        e.mov_r32_r32(Reg::RBX, Reg::R10); // mov ebx,edx
        e.add_r32_imm32(Reg::RBX, STEP2);
        e.shr_r32_imm8(Reg::RBX, 3);
        e.add_r32_imm32(Reg::R12, STEP1); // add esi,STEP1
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec edi (counter), sets ZF
        e.jnz(top);
        e.store_r32_disp8(Reg::R15, 0, Reg::R11); // out[0]=eax
        e.store_r32_disp8(Reg::R15, 4, Reg::RBX); // out[1]=ebx
        e.store_r32_disp8(Reg::R15, 8, Reg::R9); // out[2]=ecx
        e.store_r32_disp8(Reg::R15, 12, Reg::R10); // out[3]=edx
        e.store_r32_disp8(Reg::R15, 16, Reg::R8); // out[4]=ebp
        e.store_r32_disp8(Reg::R15, 20, Reg::R12); // out[5]=esi
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed")
    };
    type NativeFn = unsafe extern "C" fn(u32, *mut u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        let mut cpu = CpuGsw::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b));
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(ITERS);
        let mut mem = vec![0u8; 0x1000];
        mem[..code.len()].copy_from_slice(&code);
        let mut bus = TestBus::with_memory(mem);
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || cpu.registers.edi() == 0 {
                break;
            }
            assert!(calls < ITERS as u64 * 4 + 1000, "not converging");
        }
        let ins = t.elapsed().as_secs_f64();
        assert_eq!(cpu.registers.edi(), 0, "loop did not complete");

        let mut out = [0u32; 6];
        let t = Instant::now();
        unsafe { native_fn(ITERS, out.as_mut_ptr()) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: final registers must match.
        let interp_regs = [
            cpu.registers.eax(),
            cpu.registers.ebx(),
            cpu.registers.ecx(),
            cpu.registers.edx(),
            cpu.registers.ebp(),
            cpu.registers.esi(),
        ];
        assert_eq!(
            out, interp_regs,
            "native register-only result diverges from interpreter"
        );

        if trial > 0 {
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== Dispatch-only ceiling (register-only loop) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native      : {mn:.3} ns/guest-insn");
    eprintln!("DISPATCH SPEEDUP CEILING : {:.2}x", mi / mn);
    eprintln!("=== end dispatch-only probe ===\n");
}

/// Measure the per-slot bookkeeping cost. The register cache alone is not the limiting factor; the per-slot
/// fetch/cap CALLs back into Rust are the floor (the region is wall-neutral with the interpreter
/// precisely because native emitted code cannot inline the bus/cpu work the Rust trampoline
/// inlines). This measures the per-slot BOOKKEEPING cost (fetch charge + clock accumulate +
/// cross-multiplied cap check) under the three candidate models, the variable that decides the
/// emitted path. Combined with the native compute and interpreter
/// (~96 ns/insn) numbers, it gives the drawcolumn per-insn estimate for each model. Throwaway.
///   cargo test -j8 -p izarravm-cpu --release --features jit s2_bookkeeping -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn s2_bookkeeping_model_spike() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    #[repr(C)]
    struct SpikeState {
        raw_clocks: u64, // off 0
        bus_accum: u64,  // off 8
        cap: u64,        // off 16
        num: u64,        // off 24
    }
    const FETCH: u32 = 2; // representative RAM I-cache fetch wait-state (machine/lib.rs:9799)
    const NUM: u32 = 1; // 586 timing numerator

    // One slot's realistic bookkeeping: fetch charge + clock accumulate + cross-mult cap check.
    unsafe extern "C" fn book_one(s: *mut SpikeState) -> u8 {
        let s = unsafe { &mut *s };
        s.bus_accum += FETCH as u64;
        s.raw_clocks += 2;
        u8::from(s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap)
    }
    // The same, batched: n slots in one call (amortizes the CALL over the block iteration).
    unsafe extern "C" fn book_batch(s: *mut SpikeState, n: u32) -> u8 {
        let s = unsafe { &mut *s };
        for _ in 0..n {
            s.bus_accum += FETCH as u64;
            s.raw_clocks += 2;
            if s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap {
                return 1;
            }
        }
        0
    }
    let one_addr = (book_one as unsafe extern "C" fn(*mut SpikeState) -> u8) as usize as u64;
    let batch_addr =
        (book_batch as unsafe extern "C" fn(*mut SpikeState, u32) -> u8) as usize as u64;

    // Model 1 (today's region): one Rust bookkeeping CALL per slot. win64 arg0=RCX(state).
    let call_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32); // shadow space; RSP 16-aligned before the CALL
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.mov_r64_imm64(Reg::RBX, one_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF); // dec (sets ZF)
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 2 (Option A): bookkeeping inline, accumulators cached in host registers (no CALL).
    let native_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RSI);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.load_r64_disp8(Reg::R12, Reg::R15, 0); // raw_clocks
        e.load_r64_disp8(Reg::R13, Reg::R15, 8); // bus_accum
        e.load_r64_disp8(Reg::RSI, Reg::R15, 16); // cap
        let top = e.label();
        let exit = e.label();
        e.place(top);
        e.add_r64_imm32(Reg::R12, 2); // raw += 2
        e.add_r64_imm32(Reg::R13, FETCH); // bus += fetch cost
        e.mov_r64_r64(Reg::RAX, Reg::R12);
        e.imul_r64_imm32(Reg::RAX, NUM); // raw * num
        e.add_r64_r64(Reg::RAX, Reg::R13); // + bus term
        e.cmp_r64_r64(Reg::RAX, Reg::RSI); // vs cap
        e.jae(exit);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.place(exit);
        e.store_r64_disp8(Reg::R15, 0, Reg::R12);
        e.store_r64_disp8(Reg::R15, 8, Reg::R13);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RSI);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 3 (Option B): one Rust CALL per block-iteration doing n slots' bookkeeping.
    let batched = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters (= SLOTS / 15)
        e.mov_r64_imm64(Reg::RBX, batch_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.mov_r32_imm32(Reg::RDX, 15); // n slots / iteration
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    type Fn2 = unsafe extern "C" fn(*mut SpikeState, u64);
    const SLOTS: u64 = 15_000_000;
    const TRIALS: usize = 7;
    let run = |buf: &ExecutableBuffer, iters: u64| -> f64 {
        let f: Fn2 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        let mut st = SpikeState {
            raw_clocks: 0,
            bus_accum: 0,
            cap: u64::MAX, // never fires: every model processes all SLOTS
            num: NUM as u64,
        };
        let t = Instant::now();
        unsafe { f(&mut st, iters) };
        t.elapsed().as_secs_f64() / SLOTS as f64 * 1e9 // ns per slot
    };
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (mut c, mut n, mut b) = (Vec::new(), Vec::new(), Vec::new());
    for trial in 0..=TRIALS {
        let tc = run(&call_per_slot, SLOTS);
        let tn = run(&native_per_slot, SLOTS);
        let tb = run(&batched, SLOTS / 15);
        if trial > 0 {
            c.push(tc);
            n.push(tn);
            b.push(tb);
        }
    }
    let (mc, mn, mb) = (median(c), median(n), median(b));
    eprintln!("\n=== Bookkeeping models (ns per slot, median of {TRIALS}) ===");
    eprintln!("1. call-per-slot  (today's region model)     : {mc:.3} ns/slot");
    eprintln!("2. native-per-slot (Option A, cached accum)  : {mn:.3} ns/slot");
    eprintln!("3. batched CALL/iter (Option B, 15 slots)    : {mb:.3} ns/slot");
    eprintln!("--- drawcolumn per-insn estimate = ~0.38 ns native compute + bookkeeping/slot ---");
    eprintln!("current region (wall-neutral w/ interp)      : ~96 ns/insn");
    eprintln!(
        "Option A (native per-slot)  : {:.2} ns/insn  => {:.0}x over current",
        0.38 + mn,
        96.0 / (0.38 + mn)
    );
    eprintln!(
        "Option B (batched)          : {:.2} ns/insn  => {:.0}x over current  (+ omitted spill/reload)",
        0.38 + mb,
        96.0 / (0.38 + mb)
    );
    eprintln!("=== end bookkeeping models ===\n");
}

#[test]
fn scale_clocks_batches_exactly() {
    // The JIT accumulates raw core_clocks across a straight-line block and scales ONCE at
    // block exit. That is bit-identical to per-instruction scaling because scale_clocks is
    // exact long division with a remainder carry. Verified across every mode, several clock
    // sequences, and a non-zero starting remainder. A regression here silently breaks the
    // JIT's cyc/iter identity, so this guards the property.
    let seqs: [&[u32]; 3] = [
        &[3, 5, 1, 1, 61, 2, 7, 4, 9, 2],
        &[1; 32],
        &[255, 1, 100, 3, 17, 61, 61, 2],
    ];
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for start_rem in [0u64, 1, 7, 100] {
            for seq in seqs {
                let mut indiv = CpuGsw::default();
                indiv.set_mode(mode);
                indiv.timing_rem = start_rem;
                let mut batch = CpuGsw::default();
                batch.set_mode(mode);
                batch.timing_rem = start_rem;

                let sum_individual: u64 = seq.iter().map(|&c| indiv.scale_clocks(c)).sum();
                let total: u32 = seq.iter().sum();
                let batched = batch.scale_clocks(total);

                assert_eq!(
                    sum_individual, batched,
                    "mode {mode:?} rem {start_rem} seq {seq:?}: per-insn sum != batched"
                );
                assert_eq!(
                    indiv.timing_rem, batch.timing_rem,
                    "mode {mode:?} rem {start_rem} seq {seq:?}: remainder carry diverged"
                );
            }
        }
    }
}

/// A compiled block containing x87 operations carries `fp_rem` alongside the
/// integer `timing_rem`, and must batch both into one
/// block-exit flush with the SAME carry as per-op scaling, or the block's guest cycle count
/// diverges from the interpreter. Unlike integer clocks (one per-level numerator), FP ops have
/// PER-CLASS numerators, so the batched form weights each op by its class before summing; the
/// shared `FP_TIMING_DEN` is what keeps the single `fp_rem` carry exact across mixed classes.
/// This pins `Σ scale_fp_clocks == floor((Σ clocks·num_class + rem0) / DEN)` with the final
/// remainder `(Σ clocks·num_class + rem0) % DEN`. This guards against `scale_fp_clocks` dropping
/// the shared-denominator property the batch relies on.
#[test]
fn scale_fp_clocks_batches_exactly() {
    use FpOpClass::{F32Mem, F64Mem, IntConvert16, IntConvert32, Register, Wait};
    let seqs: [&[(u32, FpOpClass)]; 3] = [
        &[
            (4, IntConvert32),
            (1, Register),
            (3, F64Mem),
            (2, IntConvert16),
            (1, Register),
        ],
        &[(1, Register); 20],
        &[
            (7, F32Mem),
            (2, IntConvert32),
            (9, Register),
            (1, Wait),
            (5, IntConvert16),
            (3, F64Mem),
        ],
    ];
    for mode in [
        GswMode::Gsw386Slow,
        GswMode::Gsw386,
        GswMode::Gsw486,
        GswMode::Gsw586,
    ] {
        for start_rem in [0u64, 1, 5, 7] {
            for seq in seqs {
                let mut indiv = CpuGsw::default();
                indiv.set_mode(mode);
                indiv.fp_rem = start_rem;
                let sum_individual: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(indiv.scale_fp_clocks(c, cl)))
                    .sum();

                // Closed-form batched value: sum the per-op class-weighted numerators, then one
                // exact division with the single carried remainder.
                let weighted: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(c) * u64::from(fp_timing_class(mode.persona(), cl)))
                    .sum();
                let scaled = weighted + start_rem;
                let batched = scaled / u64::from(FP_TIMING_DEN);
                let final_rem = scaled % u64::from(FP_TIMING_DEN);

                assert_eq!(
                    sum_individual, batched,
                    "mode {mode:?} rem {start_rem}: per-op FP sum != batched"
                );
                assert_eq!(
                    indiv.fp_rem, final_rem,
                    "mode {mode:?} rem {start_rem}: fp_rem carry diverged"
                );
            }
        }
    }
}

#[derive(Default)]
struct TestBus {
    memory: Vec<u8>,
    trace: BusTrace,
    pending_irq: Option<u8>,
    // Mirrors the machine's `io_touched`: set by any port access, so `requires_step_break`
    // reports the same step-break edge the real bus does.
    io_touched: bool,
    // When true, `read_io` does NOT set `io_touched`, modeling the machine's
    // Approximate-class lazy status-port path (MachineBus::read_io's
    // 3DA/3BA/3C2 arm), so poll-loop chaining across an IN can be exercised
    // through the CPU alone. Writes still set io_touched (no lazy write path
    // exists on the machine either). Default false: the classic every-port-
    // access-breaks behavior.
    lazy_io_reads: bool,
    // Records the `core_clocks_so_far` value the CPU threaded into the most recent
    // `read_io` call, so tests can assert on it directly (see
    // `core_clocks_so_far_reflects_prior_instructions_not_the_in_flight_one`).
    last_read_io_core_clocks_so_far: Option<u64>,
    last_write_io_core_clocks_so_far: Option<u64>,
    // When true, `direct_page` hands out host-pointer pages into `memory` (mirroring the
    // production MachineBus), so data accesses take the CPU's cached host-pointer deref path
    // instead of the slow `read_memory_direct` fallback. Default false: the historical
    // no-direct-page behavior every existing test relies on (data accesses push trace cycles).
    // The JIT memory microbenchmark sets it true so its numbers reflect production, not the
    // slow test path (which does not exist on the real bus).
    direct_pages_enabled: bool,
    direct_pages_writable: bool,
    direct_write_denied_page: Option<u32>,
    uniform_native_fetches: bool,
    // Opt-in width-sensitive timing for direct-page tests. Historical TestBus direct pages were
    // timing-free, so keep that default and let direct-memory differential tests request clocks.
    direct_page_clocks: bool,
    // Opt-in batch-clock reporting for tight event-budget tests. Historical CPU tests leave it
    // off because their TestBus predates machine-level combined core/bus caps.
    report_batch_clocks: bool,
    page_walk_bound_available: bool,
    rep_data_byte_cost_override: Option<u64>,
    direct_memory_max_clock_override: Option<u64>,
    project_additional_bus_clocks: bool,
    native_aggregate_accounting_disabled: bool,
    jit_cached_fetch_requests: std::cell::RefCell<Vec<(u32, u32)>>,
    mode13_dirty_pages: u16,
    mode13_byte_writes: u64,
    mode13_word_writes: u64,
    mode13_dword_writes: u64,
}

impl TestBus {
    fn with_memory(memory: Vec<u8>) -> Self {
        Self {
            memory,
            trace: BusTrace::default(),
            pending_irq: None,
            io_touched: false,
            lazy_io_reads: false,
            last_read_io_core_clocks_so_far: None,
            last_write_io_core_clocks_so_far: None,
            direct_pages_enabled: false,
            direct_pages_writable: true,
            direct_write_denied_page: None,
            uniform_native_fetches: false,
            direct_page_clocks: false,
            report_batch_clocks: false,
            page_walk_bound_available: true,
            rep_data_byte_cost_override: None,
            direct_memory_max_clock_override: None,
            project_additional_bus_clocks: false,
            native_aggregate_accounting_disabled: false,
            jit_cached_fetch_requests: std::cell::RefCell::new(Vec::new()),
            mode13_dirty_pages: 0,
            mode13_byte_writes: 0,
            mode13_word_writes: 0,
            mode13_dword_writes: 0,
        }
    }

    fn direct_page_wait_states(width: BusWidth) -> u8 {
        match width {
            BusWidth::Byte => 0,
            BusWidth::Word => 1,
            BusWidth::Dword => 3,
        }
    }

    fn mode13_wait_states(width: BusWidth) -> u8 {
        match width {
            BusWidth::Byte => 4,
            BusWidth::Word => 5,
            BusWidth::Dword => 7,
        }
    }

    fn note_mode13_write(&mut self, address: u32, width: BusWidth) {
        if !(0x000a_0000..0x000b_0000).contains(&address) {
            return;
        }
        self.mode13_dirty_pages |= 1 << ((address - 0x000a_0000) >> 12);
        match width {
            BusWidth::Byte => self.mode13_byte_writes += 1,
            BusWidth::Word => self.mode13_word_writes += 1,
            BusWidth::Dword => self.mode13_dword_writes += 1,
        }
    }
}

impl CpuBus for TestBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        Ok(match width {
            BusWidth::Byte => u32::from(self.memory[start]),
            BusWidth::Word => u32::from(u16::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
            ])),
            BusWidth::Dword => u32::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
                self.memory[start + 2],
                self.memory[start + 3],
            ]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        match width {
            BusWidth::Byte => self.memory[start] = value as u8,
            BusWidth::Word => {
                self.memory[start..start + 2].copy_from_slice(&(value as u16).to_le_bytes())
            }
            BusWidth::Dword => self.memory[start..start + 4].copy_from_slice(&value.to_le_bytes()),
        }
        Ok(())
    }

    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width, kind)
            == width.bytes() as usize
        {
            return self
                .read_memory(address, width, kind)
                .map(|value| DirectMemoryRead {
                    value,
                    direct: true,
                });
        }
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width, kind)
            == width.bytes() as usize
        {
            self.write_memory(address, width, value, kind)?;
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)?;
        Ok(DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataRead {
            return Ok(0);
        }
        if self.direct_memory_bytes(address, out.len(), width, kind) != out.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..out.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        out.copy_from_slice(&self.memory[start..start + out.len()]);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if kind != BusAccessKind::DataWrite {
            return Ok(0);
        }
        if self.direct_memory_bytes(address, data.len(), width, kind) != data.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..data.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn direct_memory_bytes(
        &self,
        address: u32,
        bytes: usize,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> usize {
        if !matches!(kind, BusAccessKind::DataRead | BusAccessKind::DataWrite)
            || bytes == 0
            || bytes % width.bytes() as usize != 0
            || (address as usize & 0x0fff) + bytes > 0x1000
        {
            return 0;
        }
        if matches!(width, BusWidth::Word) && address & 1 != 0
            || matches!(width, BusWidth::Dword) && address & 3 != 0
        {
            return 0;
        }
        let start = address as usize;
        if start
            .checked_add(bytes)
            .is_some_and(|end| end <= self.memory.len())
        {
            bytes
        } else {
            0
        }
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let start = address as usize;
        if start >= self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        let len = out.len().min(self.memory.len() - start);
        out[..len].copy_from_slice(&self.memory[start..start + len]);
        Ok(len)
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    // The trait default charges a fetch run byte-by-byte (one cross-crate call + push per
    // byte), whose call overhead dominates JIT-region wall-clock microbenchmarks. This override
    // is bit-identical to that default loop in EVERY accounting field (clocks, access count,
    // Full-mode detail) but does it in one op, so no existing test changes. It does NOT
    // reproduce the production MachineBus, which collapses a cacheable-RAM run to ONE access at
    // the code-fetch wait state (this keeps `count` byte accesses at wait-state 0); the
    // microbenchmark runs tracing Off, so it measures wall clock, not the fetch-clock total.
    // Do not treat this TestBus's instruction-fetch clock accounting as production-representative.
    fn charge_physical_instruction_fetch_run(
        &mut self,
        physical_start: u32,
        count: u32,
    ) -> Result<(), BusError> {
        self.trace.record_instruction_fetch_run(
            physical_start,
            if self.uniform_native_fetches && count != 0 {
                1
            } else {
                count
            },
            0,
        );
        Ok(())
    }

    fn jit_fetch_cost_clocks(&self) -> u64 {
        u64::from(self.uniform_native_fetches) * 2
    }

    fn native_fetches_are_uniform(&self) -> bool {
        self.uniform_native_fetches
    }

    fn native_aggregate_accounting_allowed(&self) -> bool {
        !self.native_aggregate_accounting_disabled
    }

    fn jit_data_cost_clocks(&self, width: BusWidth) -> u64 {
        if self.direct_page_clocks {
            u64::from(izarravm_bus::BusCycle::clocks_for(
                width,
                Self::direct_page_wait_states(width),
            ))
        } else {
            0
        }
    }

    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        if self.report_batch_clocks {
            self.trace.elapsed_clocks()
        } else {
            0
        }
    }

    fn jit_mode13_data_cost_clocks(&self, width: BusWidth) -> u64 {
        if self.direct_page_clocks {
            u64::from(izarravm_bus::BusCycle::clocks_for(
                width,
                Self::mode13_wait_states(width),
            ))
        } else {
            0
        }
    }

    fn rep_page_walk_cost_upper(&self) -> Option<u64> {
        self.page_walk_bound_available
            .then_some(if self.report_batch_clocks { 8 } else { 0 })
    }

    fn rep_data_byte_cost_upper(&self) -> u64 {
        self.rep_data_byte_cost_override.unwrap_or_else(|| {
            self.jit_data_cost_clocks(BusWidth::Byte)
                .max(self.jit_mode13_data_cost_clocks(BusWidth::Byte))
        })
    }

    fn charge_native_mode13_writes(&mut self, writes: izarravm_bus::NativeMode13Writes) {
        self.mode13_dirty_pages |= writes.dirty_pages;
        self.mode13_byte_writes += writes.byte_writes;
        self.mode13_word_writes += writes.word_writes;
        self.mode13_dword_writes += writes.dword_writes;
        let clocks = self
            .jit_mode13_data_cost_clocks(BusWidth::Byte)
            .saturating_mul(writes.byte_writes)
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Word)
                    .saturating_mul(writes.word_writes),
            )
            .saturating_add(
                self.jit_mode13_data_cost_clocks(BusWidth::Dword)
                    .saturating_mul(writes.dword_writes),
            );
        self.trace.add_elapsed_clocks(clocks);
    }

    fn charge_bus_clocks_bulk(&mut self, clocks: u64) {
        self.trace.add_elapsed_clocks(clocks);
    }

    fn charge_direct_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        if kind == BusAccessKind::DataWrite {
            self.note_mode13_write(address, width);
        }
        if self.direct_page_clocks {
            let wait_states = if (0x000a_0000..0x000b_0000).contains(&address) {
                Self::mode13_wait_states(width)
            } else {
                Self::direct_page_wait_states(width)
            };
            self.trace.record(kind, address, width, wait_states);
        }
        Ok(())
    }

    fn jit_direct_memory_max_clocks(&self, _width: BusWidth, _kind: BusAccessKind) -> Option<u64> {
        Some(self.direct_memory_max_clock_override.unwrap_or(0))
    }

    fn jit_cached_fetch_run_clocks(&self, start: u32, count: u32) -> Option<u64> {
        self.jit_cached_fetch_requests
            .borrow_mut()
            .push((start, count));
        Some(u64::from(count) * 2)
    }

    fn jit_projected_batch_scaled_bus_clocks(&self, additional_raw: u64) -> Option<u64> {
        Some(if self.project_additional_bus_clocks {
            self.in_batch_scaled_bus_clocks()
                .saturating_add(additional_raw)
        } else {
            0
        })
    }

    // Hand out a host-pointer page into `memory`, mirroring MachineBus::direct_page, so the
    // CPU's data_read_pages/data_write_pages caches populate and subsequent accesses are host
    // derefs. Gated: off by default (the default trait None keeps every existing test on the
    // slow read_memory_direct path with its trace cycles), on only for the JIT microbenchmark.
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        if !self.direct_pages_enabled {
            return Ok(None);
        }
        let physical_page = address & !0x0fff;
        if kind == BusAccessKind::DataWrite
            && (!self.direct_pages_writable || self.direct_write_denied_page == Some(physical_page))
        {
            return Ok(None);
        }
        let start = physical_page as usize;
        if start + 0x1000 > self.memory.len() {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: 0x1000,
            writable: matches!(kind, BusAccessKind::DataWrite) && self.direct_pages_writable,
        }))
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        if !self.lazy_io_reads {
            self.io_touched = true;
        }
        self.last_read_io_core_clocks_so_far = Some(core_clocks_so_far);
        self.trace.push(BusCycle::new(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            0,
        ));
        Ok(0)
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        _value: u32,
        core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.io_touched = true;
        self.last_write_io_core_clocks_so_far = Some(core_clocks_so_far);
        self.trace.push(BusCycle::new(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            0,
        ));
        Ok(())
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    fn interrupt_pending(&self) -> bool {
        self.pending_irq.is_some()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pending_irq.take()
    }

    fn requires_step_break(&self) -> bool {
        self.io_touched
    }
}

/// Give a real-mode `TestBus` a full 1 MiB image (room for a wrap-safe stack and every
/// vector's IVT slot) and point vector 0 (#DE) at a distinguishing trap address, then run
/// one `cycle` and assert the CPU landed there -- i.e. the fault was DELIVERED through
/// `real_mode_interrupt`, not raised as a host-fatal error. Batch A converted #DE from
/// `CpuError::DivideError` (host-fatal) to `InternalFault::Exception { vector: 0, .. }`
/// (guest-deliverable), so a real-mode DIV-by-zero now runs to completion (`cycle` returns
/// `Ok`) with CS:IP retargeted at the IVT's vector-0 entry instead of erroring `cycle` itself.
const DE_TRAP_CS: u16 = 0x0200;
const DE_TRAP_IP: u16 = 0x0010;
// Code origin for the de_trap_bus helpers: away from offset 0, which is the vector-0 IVT
// slot these buses populate (code and the IVT slot must not overlap).
const DE_CODE_ORIGIN: u32 = 0x20;

fn expect_de_delivered<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) {
    let outcome = cpu
        .cycle(bus)
        .expect("a delivered #DE must not error `cycle`");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, DE_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(DE_TRAP_IP));
}

fn de_trap_bus(code: &[u8]) -> TestBus {
    let mut memory = vec![0u8; 0x1_0000];
    let origin = DE_CODE_ORIGIN as usize;
    memory[origin..origin + code.len()].copy_from_slice(code);
    // IVT[0] (bytes 0..4): IP then CS, little-endian.
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    TestBus::with_memory(memory)
}

fn cpl3_code(code: &[u8]) -> (CpuGsw, TestBus) {
    // Protected mode with a flat CPL-3 code segment (selector RPL 3), the same shape
    // the #AC/CPUID privilege tests use, but loaded with arbitrary code.
    let mut memory = vec![0u8; 256];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    // This helper builds CPL-3 state directly (no transfer instruction runs), so the
    // cached `cpl` must be seeded to match the CS RPL it just installed by hand.
    cpu.cpl = 3;
    (cpu, TestBus::with_memory(memory))
}

/// Run one instruction through the production decode/execute split and return the raw
/// `InternalFault` (without exception delivery), so a test can assert `is_ok()`/`unwrap_err()`
/// directly on the result. This is the single per-instruction entry the test suite uses now that
/// the transitional fused reference is gone: it is exactly what `cycle` runs, minus the
/// interrupt-service prologue and the exception-delivery epilogue.
fn exec_one_split<B: CpuBus>(cpu: &mut CpuGsw, bus: &mut B) -> ExecResult<CycleOutcome> {
    cpu.begin_instruction();
    let insn = cpu.decode(bus)?;
    cpu.execute_decoded(&insn, bus)
}

fn real_mode_cpu(code: &[u8], mem_len: usize) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; mem_len];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    (cpu, memory)
}

/// Shared seed for the seam differential / golden batteries below: a fixed real-mode register
/// set plus a known word at [0x20], so each instruction has stable inputs.
fn seam_seed(cpu: &mut CpuGsw) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0102);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Cx, 0x0304);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.write_reg16(Reg16::Bp, 0x0010);
}

fn seam_fetch_count(bus: &TestBus) -> usize {
    bus.trace
        .cycles()
        .iter()
        .filter(|c| c.kind == BusAccessKind::InstructionPrefetch)
        .count()
}

/// Protected-mode CPU with a GDT (base 0x100, limit 0x1f) holding one descriptor
/// at selector 0x08. CS selector 0 => CPL 0.
fn protected_cpu(code: &[u8], descriptor_low: u32, descriptor_high: u32) -> (CpuGsw, Vec<u8>) {
    let mut memory = vec![0u8; 0x200];
    memory[..code.len()].copy_from_slice(code);
    memory[0x108..0x10c].copy_from_slice(&descriptor_low.to_le_bytes());
    memory[0x10c..0x110].copy_from_slice(&descriptor_high.to_le_bytes());
    let mut cpu = CpuGsw::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0x1f,
    };
    cpu.control.cr0 |= CR0_PE;
    (cpu, memory)
}

// ---- V86 monitor test harness -------------------------------------------------
// Memory map (physical == linear, identity paged):
//   0x00000 IVT area; 0x01000 page directory; 0x02000 page table 0 (identity,
//   present+rw+user); 0x03000 GDT; 0x04000 IDT; 0x05000 TSS (+ I/O bitmap);
//   ESP0 = 0x07000 (ring-0 stack, flat SS base 0); 0x08000 monitor code;
//   V86 guest: SS=0x0900, CS=0x0A00 (code at phys 0xA000).
// GDT selectors: 0x08 ring0 code (32-bit), 0x10 ring0 data/stack, 0x18 TSS.
const GDT: u32 = 0x3000;
const IDT: u32 = 0x4000;
const TSS: u32 = 0x5000;
const R0_CS: u16 = 0x08;
const R0_SS: u16 = 0x10;
const TSS_SEL: u16 = 0x18;
const MON_CODE: u32 = 0x8000;
const ESP0: u32 = 0x7000;

fn put32(m: &mut [u8], off: u32, v: u32) {
    m[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
}
fn put16(m: &mut [u8], off: u32, v: u16) {
    m[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes());
}
fn descriptor(base: u32, limit: u32, access: u8, gran: u8) -> [u8; 8] {
    let mut d = [0u8; 8];
    d[0..2].copy_from_slice(&(limit as u16).to_le_bytes());
    d[2..4].copy_from_slice(&(base as u16).to_le_bytes());
    d[4] = (base >> 16) as u8;
    d[5] = access;
    d[6] = ((limit >> 16) as u8 & 0x0f) | (gran & 0xf0);
    d[7] = (base >> 24) as u8;
    d
}
fn int_gate(m: &mut [u8], vector: u8, offset: u32) {
    let base = IDT + u32::from(vector) * 8;
    put16(m, base, offset as u16);
    put16(m, base + 2, R0_CS);
    m[base as usize + 4] = 0;
    m[base as usize + 5] = 0x8e; // present, DPL0, 32-bit interrupt gate
    put16(m, base + 6, (offset >> 16) as u16);
}
fn cpu_mem(bus: &TestBus, addr: u32) -> [u8; 4] {
    let a = addr as usize;
    [
        bus.memory[a],
        bus.memory[a + 1],
        bus.memory[a + 2],
        bus.memory[a + 3],
    ]
}

/// Build the world; CPU sits in protected mode + paging with TR/GDTR/IDTR loaded.
fn v86_world(monitor: &[u8], guest: &[u8], io_bitmap: &[u8]) -> (CpuGsw, TestBus) {
    let mut m = vec![0u8; 0x20000];
    // Identity paging: PDE[0] -> PT at 0x2000; first 0x20 pages identity present+rw+user.
    put32(&mut m, 0x1000, 0x2000 | 0x7);
    for i in 0..0x20u32 {
        put32(&mut m, 0x2000 + i * 4, (i << 12) | 0x7);
    }
    // GDT: null (offset 0), ring0 code 0x9b (sel 0x08), ring0 data 0x93 (sel 0x10),
    // TSS 0x89 (sel 0x18).
    let d = descriptor(0, 0xfffff, 0x9b, 0xc0);
    m[(GDT + 0x08) as usize..(GDT + 0x08) as usize + 8].copy_from_slice(&d);
    let d = descriptor(0, 0xfffff, 0x93, 0xc0);
    m[(GDT + 0x10) as usize..(GDT + 0x10) as usize + 8].copy_from_slice(&d);
    let tss_limit = 0x68 + io_bitmap.len() as u32;
    let d = descriptor(TSS, tss_limit, 0x89, 0x00);
    m[(GDT + 0x18) as usize..(GDT + 0x18) as usize + 8].copy_from_slice(&d);
    // TSS: ESP0, SS0, I/O-map base (word at TSS+0x66), bitmap.
    put32(&mut m, TSS + 4, ESP0);
    put16(&mut m, TSS + 8, R0_SS);
    put16(&mut m, TSS + 0x66, 0x68);
    m[(TSS + 0x68) as usize..(TSS + 0x68) as usize + io_bitmap.len()].copy_from_slice(io_bitmap);
    // IDT: #GP (13) and INT 0x21 -> monitor.
    int_gate(&mut m, 13, MON_CODE);
    int_gate(&mut m, 0x21, MON_CODE);
    m[MON_CODE as usize..MON_CODE as usize + monitor.len()].copy_from_slice(monitor);
    m[0xA000..0xA000 + guest.len()].copy_from_slice(guest);

    let mut cpu = CpuGsw::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.gdtr.base = GDT;
    cpu.gdtr.limit = 0xff;
    cpu.idtr.base = IDT;
    cpu.idtr.limit = 0xfff;
    cpu.tr = SegmentRegister {
        selector: TSS_SEL,
        base: TSS,
        limit: tss_limit,
        access: 0x89,
        default_size_32: false,
    };
    let bus = TestBus::with_memory(m);
    (cpu, bus)
}

/// Put `cpu` into a V86 task at CS:IP=0x0A00:ip, SS:SP=0x0900:sp, IOPL 0.
/// DS/ES/FS/GS are seeded with sensible defaults; a caller may overwrite them
/// afterward to probe the V86 segment frame (none of them are load-bearing here).
fn enter_v86_direct(cpu: &mut CpuGsw, ip: u32, sp: u32) {
    cpu.registers.eflags = (cpu.registers.eflags & !0x3000) | FLAG_VM | 0x2;
    cpu.registers.eip = ip;
    cpu.registers.set_esp(sp);
    cpu.load_segment_real(SegmentIndex::Cs, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Ss, 0x0900);
    cpu.load_segment_real(SegmentIndex::Ds, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Es, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Fs, 0);
    cpu.load_segment_real(SegmentIndex::Gs, 0);
    // This helper sets EFLAGS.VM directly (no IRET/task-switch transition runs), so
    // the cached `cpl` must be seeded to the fixed V86 level by hand, same as a real
    // transition would leave it.
    cpu.cpl = 3;
}

#[path = "cpu_alu_data_test.rs"]
mod alu_data;
#[path = "cpu_bit_control_test.rs"]
mod bit_control;
#[path = "cpu_bit_system_test.rs"]
mod bit_system;
#[path = "cpu_control_string_test.rs"]
mod control_string;
#[path = "cpu_execution_test.rs"]
mod execution;
#[path = "cpu_fpu_flags_test.rs"]
mod fpu_flags;
#[path = "cpu_legacy_system_test.rs"]
mod legacy_system;
#[path = "cpu_persona_system_test.rs"]
mod persona_system;
#[path = "cpu_stack_branch_test.rs"]
mod stack_branch;
#[path = "cpu_straight_line_test.rs"]
mod straight_line;
#[path = "cpu_strings_segments_test.rs"]
mod strings_segments;
#[path = "cpu_v86_test.rs"]
mod v86;

/// Differential tests for the compiled loop-region.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_region_test.rs"]
mod jit_region;

/// Differential tests for the generic JIT block builder.
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_general_test.rs"]
mod jit_general;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_direct_test.rs"]
mod jit_direct;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_compile_outcome_test.rs"]
mod jit_compile_outcome;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_differential_generator_test.rs"]
mod jit_differential_generator;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_x87_direct_test.rs"]
mod jit_x87_direct;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_double_shift_test.rs"]
mod jit_double_shift;

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[path = "cpu_jit_test_imm_test.rs"]
mod jit_test_imm;
