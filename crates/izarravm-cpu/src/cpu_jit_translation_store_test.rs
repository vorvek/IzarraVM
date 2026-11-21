// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! A NATIVE store to a page-table page that a walk already read.
//!
//! The page-table page is marked as translation structure (`DecodeCache::translation_pages`), so
//! its fast-map entry carries `PAGE_WATCHED` and a poisoned store bias. A code review claimed that
//! such a store can finish inside the block without the retire the interpreter runs for the same
//! store (`note_code_write`, `range_hits_translation_page`), because the slow stub reads only the
//! two code-watch tables. This file runs the same store as an interpreted program and as a
//! compiled block, and compares what each leg leaves behind.

use super::*;

const ENTRY: u32 = 0x1000;
const PAGE_DIRECTORY_AT: u32 = 0x3000;
const PAGE_TABLE_AT: u32 = 0x4000;
/// The data page the store re-maps. Its byte reads 0x11 through the old mapping.
const OLD_PAGE: u32 = 0x8000;
/// The page the store re-maps it to. Its byte reads 0x22.
const NEW_PAGE: u32 = 0x9000;
/// Linear address of the page-table entry for `OLD_PAGE`. The table maps itself at `PAGE_TABLE_AT`
/// with write permission, so a store to this address writes the entry.
const PTE_ADDRESS: u32 = PAGE_TABLE_AT + (OLD_PAGE >> 12) * 4;
const OLD_PTE: u32 = OLD_PAGE | 0x23;
const NEW_PTE: u32 = NEW_PAGE | 0x23;
const MEMORY_BYTES: usize = 0x20_000;
/// A plain data page the priming runs store to.
const PLAIN_PAGE: u32 = 0x6000;

/// `nop; mov [edi], edx; mov al, [OLD_PAGE]; mov ecx, eax; test ecx, ecx; jnz +1; hlt; hlt`. The
/// priming runs point EDI at a plain data page, so the block compiles without a translation-page
/// store retiring it. The measured run points EDI at the page-table entry. The load after the
/// store shows which mapping the block sees.
fn program() -> Vec<u8> {
    let mut code = vec![0x90, 0x89, 0x17, 0xa0];
    code.extend_from_slice(&OLD_PAGE.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xc1, 0x85, 0xc9, 0x75, 1, 0xf4, 0xf4]);
    code
}

/// Present, writable, accessed (and dirty for the table's own page, so a write through it needs no
/// walk to set a bit). Every page below 0x20000 maps to itself.
fn pristine_memory() -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_BYTES];
    let code = program();
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let pde = (PAGE_TABLE_AT | 0x23).to_le_bytes();
    memory[PAGE_DIRECTORY_AT as usize..PAGE_DIRECTORY_AT as usize + 4].copy_from_slice(&pde);
    for page in 0..32u32 {
        let flags = if page == PAGE_TABLE_AT >> 12 {
            0x63
        } else {
            0x23
        };
        let at = (PAGE_TABLE_AT + page * 4) as usize;
        memory[at..at + 4].copy_from_slice(&((page << 12) | flags).to_le_bytes());
    }
    memory[OLD_PAGE as usize] = 0x11;
    memory[NEW_PAGE as usize] = 0x22;
    memory
}

fn paged_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.control.cr0 |= CR0_PE | CR0_PG | CR0_WP;
    cpu.control.cr3 = PAGE_DIRECTORY_AT;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0008, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x0010, 0x93));
    }
    cpu
}

fn arm(cpu: &mut CpuGsw, edi: u32, edx: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr = [0; 8];
    cpu.registers.gpr[4] = 0x1_f000;
    cpu.registers.gpr[7] = edi;
    cpu.registers.gpr[2] = edx;
    cpu.registers.eflags = 0x202;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
}

fn run_to_halt(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for _ in 0..64 {
        if cpu
            .run_budgeted(bus, 256)
            .expect("the program must run")
            .halted
        {
            return;
        }
    }
    panic!("the program did not halt");
}

/// What one leg looked like before the measured run, and what it left behind after.
#[derive(Debug)]
struct Leg {
    /// The table page has a live fast-map write mapping with `PAGE_WATCHED` set and a poisoned
    /// store bias: the state the review describes.
    table_entry_watched_and_poisoned: bool,
    blocks_before: usize,
    native_insns: u64,
    translation_page_writes: u64,
    code_invalidations: u64,
    pte_after: u32,
    /// What the block's own load of the old page returned, after its store.
    block_saw: u8,
    /// What a later interpreted load of the old page returns.
    read_back_after: u8,
}

fn run_leg(native: bool) -> Leg {
    let pristine = pristine_memory();
    let mut cpu = paged_cpu();
    let mut bus = TestBus::with_memory(pristine.clone());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.flat_direct_page_clocks = true;
    cpu.set_jit_auto_admit(false);
    let restore = |bus: &mut TestBus| {
        bus.memory.copy_from_slice(&pristine);
        bus.trace.clear();
    };
    // Priming: one interpreted run, then four more with admission on, so the native leg compiles
    // the block. The interpreted leg never turns admission on.
    arm(&mut cpu, PLAIN_PAGE, 0);
    run_to_halt(&mut cpu, &mut bus);
    if native {
        cpu.set_jit_auto_admit(true);
        let blocks = cpu.jit_direct.len();
        for _ in 0..4 {
            restore(&mut bus);
            arm(&mut cpu, PLAIN_PAGE, 0);
            run_to_halt(&mut cpu, &mut bus);
        }
        assert!(cpu.jit_direct.len() > blocks, "the block did not compile");
    }

    // The measured run. Memory is back to the old mapping, so drop every translation the priming
    // runs cached. The compiled block and the table-page marks stay. Then warm the table page's
    // fast-map entry with a store that writes the entry it already holds (the store changes
    // nothing, so it retires nothing and keeps the block), and warm the old page's TLB entry.
    restore(&mut bus);
    cpu.tlb.flush_live_slot();
    cpu.jit_fast_map.invalidate_all();
    arm(&mut cpu, PTE_ADDRESS, NEW_PTE);
    cpu.write_memory_sized(
        &mut bus,
        SegmentIndex::Ds,
        PTE_ADDRESS,
        OperandSize::Dword,
        OLD_PTE,
        BusAccessKind::DataWrite,
    )
    .expect("the warm-up store must retire");
    assert_eq!(
        cpu.read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            OLD_PAGE,
            BusAccessKind::DataRead
        )
        .expect("the old page reads"),
        0x11
    );
    assert!(
        cpu.tlb.lookup(OLD_PAGE >> 12).is_some(),
        "the old page is warm"
    );
    let table_entry_watched_and_poisoned = cpu
        .jit_fast_map
        .has_write_mapping(PAGE_TABLE_AT, PAGE_TABLE_AT)
        && cpu.jit_fast_map.page_watched_bit_for_test(PAGE_TABLE_AT)
        && cpu.jit_fast_map.store_bias_for_test(PAGE_TABLE_AT)
            == jit::fast_map::NATIVE_STORE_BIAS_POISON;
    let blocks_before = cpu.jit_direct.len();

    let before = cpu.perf_counters().clone();
    run_to_halt(&mut cpu, &mut bus);
    let after = cpu.perf_counters().clone();
    let pte_after = u32::from_le_bytes(
        bus.memory[PTE_ADDRESS as usize..PTE_ADDRESS as usize + 4]
            .try_into()
            .unwrap(),
    );
    let block_saw = cpu.registers.gpr[1] as u8;
    let read_back_after = cpu
        .read_memory_u8(
            &mut bus,
            SegmentIndex::Ds,
            OLD_PAGE,
            BusAccessKind::DataRead,
        )
        .expect("the re-mapped page reads");
    Leg {
        table_entry_watched_and_poisoned,
        blocks_before,
        native_insns: after.jit_direct_insns - before.jit_direct_insns,
        translation_page_writes: after.translation_page_writes - before.translation_page_writes,
        code_invalidations: after.code_invalidations - before.code_invalidations,
        pte_after,
        block_saw,
        read_back_after,
    }
}

/// The reproduction holds: the store really runs inside a compiled block, through a fast-map
/// entry that is watched and has a poisoned store bias, and it writes the page-table entry.
#[test]
fn the_native_leg_runs_the_translation_page_store_inside_a_block() {
    let native = run_leg(true);
    assert!(native.table_entry_watched_and_poisoned, "{native:#?}");
    assert!(native.blocks_before > 0, "{native:#?}");
    assert!(
        native.native_insns >= 3,
        "the block must retire its instructions natively: {native:#?}"
    );
    assert_eq!(native.pte_after, NEW_PTE, "{native:#?}");
}

/// The interpreter retires on this store: one translation-page write, one invalidation, and the
/// next load of the old page sees the new mapping.
#[test]
fn the_interpreter_retires_on_the_translation_page_store() {
    let interpreted = run_leg(false);
    assert_eq!(interpreted.native_insns, 0, "{interpreted:#?}");
    assert_eq!(interpreted.translation_page_writes, 1, "{interpreted:#?}");
    assert!(interpreted.code_invalidations >= 1, "{interpreted:#?}");
    assert_eq!(interpreted.pte_after, NEW_PTE, "{interpreted:#?}");
    assert_eq!(interpreted.block_saw, 0x22, "{interpreted:#?}");
    assert_eq!(interpreted.read_back_after, 0x22, "{interpreted:#?}");
}

/// The review's claim, measured on 2026-11-21: the native leg writes the entry but does not
/// retire (no translation-page write, no invalidation), and both the block's own load and a later
/// load still see the OLD mapping through the stale TLB entry. The interpreter leg retires and
/// sees the new mapping. Ignored so the suite stays green; remove the ignore with the fix.
#[test]
#[ignore = "native store to a translation page skips the ring retire"]
fn a_native_store_to_a_translation_page_retires_like_the_interpreter() {
    let interpreted = run_leg(false);
    let native = run_leg(true);
    assert!(native.native_insns >= 3, "{native:#?}");
    assert_eq!(native.pte_after, interpreted.pte_after);
    assert_eq!(
        native.translation_page_writes, interpreted.translation_page_writes,
        "native {native:#?} against interpreted {interpreted:#?}"
    );
    assert_eq!(
        native.code_invalidations, interpreted.code_invalidations,
        "native {native:#?} against interpreted {interpreted:#?}"
    );
    assert_eq!(native.block_saw, 0x22, "{native:#?}");
    assert_eq!(native.read_back_after, 0x22, "{native:#?}");
}
