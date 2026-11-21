// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! A NATIVE store to a page-table page that a walk already read.
//!
//! The page-table page is marked as translation structure (`DecodeCache::translation_pages`). The
//! interpreter retires the decode ring, the TLB generations and the fast map when a store hits such
//! a page (`note_code_write`, `range_hits_translation_page`). A native store must not finish inside
//! the block without that retire. The fast map therefore holds no write mapping for a translation
//! page, so every native store form leaves the block and the interpreter runs the store. This file
//! runs the same store as an interpreted program and as a compiled block, and compares what each
//! leg leaves behind.

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
    store_program(&[0x89, 0x17])
}

/// The same program with another store instruction in place of `mov [edi], edx`.
fn store_program(store: &[u8]) -> Vec<u8> {
    let mut code = vec![0x90];
    code.extend_from_slice(store);
    code.push(0xa0);
    code.extend_from_slice(&OLD_PAGE.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xc1, 0x85, 0xc9, 0x75, 1, 0xf4, 0xf4]);
    code
}

/// `add dword [edi], 0x1000`: the old entry plus 0x1000 is the new entry.
fn add_store_program() -> Vec<u8> {
    store_program(&[0x81, 0x07, 0x00, 0x10, 0x00, 0x00])
}

/// `or [edi], edx`: the old entry or the new entry is the new entry.
fn or_store_program() -> Vec<u8> {
    store_program(&[0x09, 0x17])
}

/// `nop; mov ecx, [edi]; test ecx, ecx; jnz +1; hlt; hlt`: a load from the page-table entry.
fn load_program() -> Vec<u8> {
    vec![0x90, 0x8b, 0x0f, 0x85, 0xc9, 0x75, 1, 0xf4, 0xf4]
}

/// The second page directory. Its first entry names the same page table as the first directory, so
/// both directories translate every address the same way.
const DIRECTORY_B_AT: u32 = 0x5000;
/// `mov cr3, eax`, run by the interpreter to switch directories.
const CR3_CODE_AT: u32 = 0x2000;
/// A second linear page that maps the page-table page, so two linear pages alias it.
const ALIAS_LINEAR: u32 = 0xa000;

/// What a leg changes against the default store program with the default CPU settings.
#[derive(Clone)]
struct Config {
    program: Vec<u8>,
    partition: Option<bool>,
    two_tables: Option<bool>,
    /// Run `MOV CR3` to the second directory and back before the final load.
    cr3_round_trip: bool,
    /// Fill the fast map read mapping of the page-table entry before the measured run.
    warm_read: bool,
    /// Map the page-table page at `ALIAS_LINEAR` too.
    alias: bool,
}

impl Config {
    fn new() -> Self {
        Self {
            program: program(),
            partition: None,
            two_tables: None,
            cr3_round_trip: false,
            warm_read: false,
            alias: false,
        }
    }

    fn with_program(program: Vec<u8>) -> Self {
        Self {
            program,
            ..Self::new()
        }
    }

    /// Both CR3 knobs off, the one-table layout.
    fn one_table(mut self) -> Self {
        self.partition = Some(false);
        self.two_tables = Some(false);
        self
    }

    fn round_trip(mut self) -> Self {
        self.cr3_round_trip = true;
        self
    }
}

/// Present, writable, accessed (and dirty for the table's own page, so a write through it needs no
/// walk to set a bit). Every page below 0x20000 maps to itself.
fn pristine_memory_for(config: &Config) -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_BYTES];
    let code = &config.program;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(code);
    memory[CR3_CODE_AT as usize..CR3_CODE_AT as usize + 3].copy_from_slice(&[0x0f, 0x22, 0xd8]);
    let pde = (PAGE_TABLE_AT | 0x23).to_le_bytes();
    for directory in [PAGE_DIRECTORY_AT, DIRECTORY_B_AT] {
        memory[directory as usize..directory as usize + 4].copy_from_slice(&pde);
    }
    for page in 0..32u32 {
        let flags = if page == PAGE_TABLE_AT >> 12 {
            0x63
        } else {
            0x23
        };
        let at = (PAGE_TABLE_AT + page * 4) as usize;
        memory[at..at + 4].copy_from_slice(&((page << 12) | flags).to_le_bytes());
    }
    if config.alias {
        let at = (PAGE_TABLE_AT + (ALIAS_LINEAR >> 12) * 4) as usize;
        memory[at..at + 4].copy_from_slice(&(PAGE_TABLE_AT | 0x63).to_le_bytes());
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
    /// The page-table page has a live fast-map write mapping after the warm-up.
    table_write_mapping: bool,
    blocks_before: usize,
    native_insns: u64,
    /// Entries into a compiled block. A block that leaves at its store retires no counted insns.
    native_entries: u64,
    translation_page_writes: u64,
    code_invalidations: u64,
    /// Side exits the block took because the fast map had no usable mapping for an access.
    exits_unavailable: u64,
    /// Write fills the fast map refused during the warm-up store.
    refused_warmup: u64,
    /// Write fills the fast map refused during the measured run.
    refused_run: u64,
    pte_after: u32,
    /// What the block's own load of the old page returned, after its store.
    block_saw: u8,
    ecx: u32,
    /// What a later interpreted load of the old page returns.
    read_back_after: u8,
}

struct Rig {
    cpu: CpuGsw,
    bus: TestBus,
    pristine: Vec<u8>,
}

impl Rig {
    fn restore(&mut self) {
        self.bus.memory.copy_from_slice(&self.pristine);
        self.bus.trace.clear();
    }

    /// Memory is back to the old mapping, so drop every translation the priming runs cached. The
    /// compiled block and the table-page marks stay.
    fn drop_translations(&mut self) {
        self.restore();
        self.cpu.tlb.flush_live_slot();
        self.cpu.jit_fast_map.invalidate_all();
    }
}

fn refused(cpu: &CpuGsw) -> u64 {
    cpu.fast_map_audit_counters()
        .fast_map_write_fill_refused_translation
}

/// Run the program as the priming runs: one interpreted run, then four more with admission on for
/// the native leg, so it compiles the block. The interpreted leg never turns admission on.
fn primed(native: bool, config: &Config) -> Rig {
    crate::paging::set_tlb_slot_partition_for_test(config.partition);
    crate::jit::fast_map::set_fast_map_two_tables_for_test(config.two_tables);
    let mut cpu = paged_cpu();
    crate::paging::set_tlb_slot_partition_for_test(None);
    crate::jit::fast_map::set_fast_map_two_tables_for_test(None);
    let pristine = pristine_memory_for(config);
    let mut bus = TestBus::with_memory(pristine.clone());
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus.flat_direct_page_clocks = true;
    cpu.set_jit_auto_admit(false);
    let mut rig = Rig { cpu, bus, pristine };
    arm(&mut rig.cpu, PLAIN_PAGE, 0);
    run_to_halt(&mut rig.cpu, &mut rig.bus);
    if native {
        rig.cpu.set_jit_auto_admit(true);
        let blocks = rig.cpu.jit_direct.len();
        for _ in 0..4 {
            rig.restore();
            arm(&mut rig.cpu, PLAIN_PAGE, 0);
            run_to_halt(&mut rig.cpu, &mut rig.bus);
        }
        assert!(
            rig.cpu.jit_direct.len() > blocks,
            "the block did not compile"
        );
    }
    rig
}

/// `MOV CR3, value`, run by the interpreter.
fn set_cr3(cpu: &mut CpuGsw, bus: &mut TestBus, value: u32) {
    cpu.registers.set_eax(value);
    cpu.set_eip(CR3_CODE_AT);
    exec_one_split(cpu, bus).expect("MOV CR3 must retire");
}

fn read_byte(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) -> u8 {
    cpu.read_memory_u8(bus, SegmentIndex::Ds, linear, BusAccessKind::DataRead)
        .expect("the read must not fault")
}

/// Store the entry value it already holds, so the store changes nothing and retires nothing.
fn store_old_entry(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32) {
    cpu.write_memory_sized(
        bus,
        SegmentIndex::Ds,
        linear,
        OperandSize::Dword,
        OLD_PTE,
        BusAccessKind::DataWrite,
    )
    .expect("the store must retire");
}

fn run_leg(native: bool) -> Leg {
    run_leg_with(native, &Config::new())
}

fn run_leg_with(native: bool, config: &Config) -> Leg {
    measure(primed(native, config), config)
}

fn measure(mut rig: Rig, config: &Config) -> Leg {
    // The measured run. Warm the page-table page's fast-map entry with a store that writes the
    // entry it already holds, and warm the old page's TLB entry.
    rig.drop_translations();
    let Rig { cpu, bus, .. } = &mut rig;
    arm(cpu, PTE_ADDRESS, NEW_PTE);
    let refused_before_warmup = refused(cpu);
    store_old_entry(cpu, bus, PTE_ADDRESS);
    let refused_warmup = refused(cpu) - refused_before_warmup;
    if config.warm_read {
        read_byte(cpu, bus, PTE_ADDRESS);
    }
    assert_eq!(read_byte(cpu, bus, OLD_PAGE), 0x11);
    assert!(
        cpu.tlb.lookup(OLD_PAGE >> 12).is_some(),
        "the old page is warm"
    );
    let table_write_mapping = cpu
        .jit_fast_map
        .has_write_mapping(PAGE_TABLE_AT, PAGE_TABLE_AT);
    let blocks_before = cpu.jit_direct.len();

    let before = cpu.perf_counters().clone();
    let refused_before_run = refused(cpu);
    run_to_halt(cpu, bus);
    let after = cpu.perf_counters().clone();
    let refused_run = refused(cpu) - refused_before_run;
    let pte_after = u32::from_le_bytes(
        bus.memory[PTE_ADDRESS as usize..PTE_ADDRESS as usize + 4]
            .try_into()
            .unwrap(),
    );
    let block_saw = cpu.registers.gpr[1] as u8;
    let ecx = cpu.registers.gpr[1];
    if config.cr3_round_trip {
        set_cr3(cpu, bus, DIRECTORY_B_AT);
        set_cr3(cpu, bus, PAGE_DIRECTORY_AT);
    }
    let read_back_after = read_byte(cpu, bus, OLD_PAGE);
    Leg {
        table_write_mapping,
        blocks_before,
        native_insns: after.jit_direct_insns - before.jit_direct_insns,
        native_entries: after.jit_direct_entries - before.jit_direct_entries,
        translation_page_writes: after.translation_page_writes - before.translation_page_writes,
        code_invalidations: after.code_invalidations - before.code_invalidations,
        exits_unavailable: after.jit_direct_exit_unavailable_or_kind
            - before.jit_direct_exit_unavailable_or_kind,
        refused_warmup,
        refused_run,
        pte_after,
        block_saw,
        ecx,
        read_back_after,
    }
}

/// The page-table page has no write mapping after the warm-up, so the store leaves the block
/// exactly once, and the page-table entry still changes.
#[test]
fn the_native_leg_leaves_the_block_at_the_translation_page_store() {
    let native = run_leg(true);
    assert!(!native.table_write_mapping, "{native:#?}");
    assert!(native.blocks_before > 0, "{native:#?}");
    assert!(
        native.native_entries >= 1,
        "the block must start natively: {native:#?}"
    );
    assert_eq!(native.exits_unavailable, 1, "{native:#?}");
    assert_eq!(native.refused_warmup, 1, "{native:#?}");
    assert_eq!(native.refused_run, 1, "{native:#?}");
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

/// The native leg and the interpreted leg leave the same guest-visible state.
fn assert_legs_agree(config: &Config) {
    let interpreted = run_leg_with(false, config);
    let native = run_leg_with(true, config);
    assert!(native.native_entries >= 1, "{native:#?}");
    assert_eq!(native.exits_unavailable, 1, "{native:#?}");
    assert_eq!(native.pte_after, interpreted.pte_after);
    assert_eq!(native.pte_after, NEW_PTE, "{native:#?}");
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
    assert_eq!(interpreted.read_back_after, 0x22, "{interpreted:#?}");
}

#[test]
fn a_native_store_to_a_translation_page_retires_like_the_interpreter() {
    assert_legs_agree(&Config::new());
}

/// A store, a switch to the other directory and back, then a load through the re-mapped page. The
/// load must read the new mapping, with the defaults and with both CR3 knobs off.
#[test]
fn a_native_pte_store_survives_a_directory_round_trip() {
    assert_legs_agree(&Config::new().round_trip());
    assert_legs_agree(&Config::new().one_table().round_trip());
}

/// A read-modify-write store to the entry takes the same path as the plain store.
#[test]
fn read_modify_write_stores_to_a_translation_page_retire_like_the_interpreter() {
    assert_legs_agree(&Config::with_program(add_store_program()));
    assert_legs_agree(&Config::with_program(or_store_program()));
}

/// Make the page-table page hold write mappings under `linears` while it is not a translation
/// page: warm the TLB through reads, clear the mark, then fill the write mappings by a store that
/// changes nothing. With `code_watched` the page is code watched before the fills.
fn stage_write_mappings(rig: &mut Rig, linears: &[u32], code_watched: bool) {
    let Rig { cpu, bus, .. } = rig;
    assert_eq!(read_byte(cpu, bus, OLD_PAGE), 0x11);
    for &linear in linears {
        read_byte(cpu, bus, linear);
    }
    let page = PAGE_TABLE_AT >> 12;
    cpu.decode_cache.translation_pages[(page >> 6) as usize] &= !(1u64 << (page & 63));
    if code_watched {
        cpu.mark_decode_code_for_test(PTE_ADDRESS, 4);
    }
    for &linear in linears {
        store_old_entry(cpu, bus, linear);
        assert!(
            cpu.jit_fast_map.has_write_mapping(linear, PAGE_TABLE_AT),
            "the staged write mapping of {linear:#x} is missing"
        );
    }
}

/// The page-table page is code watched first, and becomes a translation page afterwards. No write
/// mapping may remain, and a native store exits and retires.
#[test]
fn a_code_watched_page_that_becomes_a_translation_page_loses_its_write_mapping() {
    let mut rig = primed(true, &Config::new());
    rig.drop_translations();
    stage_write_mappings(&mut rig, &[PTE_ADDRESS], true);
    assert!(rig.cpu.jit_fast_map.page_watched_bit_for_test(PTE_ADDRESS));
    rig.cpu.mark_translation_page(PAGE_TABLE_AT);
    assert!(
        !rig.cpu
            .jit_fast_map
            .has_write_mapping(PTE_ADDRESS, PAGE_TABLE_AT)
    );
    assert!(
        rig.cpu.jit_direct.len() > 0,
        "the block must survive the staging"
    );
    let Rig { cpu, bus, .. } = &mut rig;
    arm(cpu, PTE_ADDRESS, NEW_PTE);
    let before = cpu.perf_counters().clone();
    run_to_halt(cpu, bus);
    let after = cpu.perf_counters().clone();
    assert_eq!(
        after.jit_direct_exit_unavailable_or_kind - before.jit_direct_exit_unavailable_or_kind,
        1
    );
    assert_eq!(
        after.translation_page_writes - before.translation_page_writes,
        1
    );
    assert_eq!(cpu.registers.gpr[1] as u8, 0x22);
}

/// Two linear pages map the page-table page. When it becomes a translation page, neither keeps a
/// write mapping.
#[test]
fn every_alias_of_a_translation_page_loses_its_write_mapping() {
    let config = Config {
        alias: true,
        ..Config::new()
    };
    let mut rig = primed(true, &config);
    rig.drop_translations();
    let alias_entry = ALIAS_LINEAR + (PTE_ADDRESS & 0xfff);
    stage_write_mappings(&mut rig, &[PTE_ADDRESS, alias_entry], true);
    rig.cpu.mark_translation_page(PAGE_TABLE_AT);
    for linear in [PTE_ADDRESS, alias_entry] {
        assert!(
            !rig.cpu
                .jit_fast_map
                .has_write_mapping(linear, PAGE_TABLE_AT),
            "{linear:#x} keeps a write mapping"
        );
    }
}

/// The write mapping sits in the dormant table when the page becomes a translation page. After the
/// switch back, the live table has none.
#[test]
fn the_dormant_table_loses_its_write_mapping_of_a_translation_page() {
    let config = Config {
        partition: Some(true),
        two_tables: Some(true),
        ..Config::new()
    };
    let mut rig = primed(true, &config);
    rig.drop_translations();
    // Warm the code fetch of the CR3 instruction, so the switch runs no walk of its own.
    set_cr3(&mut rig.cpu, &mut rig.bus, PAGE_DIRECTORY_AT);
    stage_write_mappings(&mut rig, &[PTE_ADDRESS], true);
    set_cr3(&mut rig.cpu, &mut rig.bus, DIRECTORY_B_AT);
    assert!(
        rig.cpu
            .jit_fast_map
            .dormant_has_write_mapping_for_test(PTE_ADDRESS),
        "the write mapping must sit in the dormant table"
    );
    rig.cpu.mark_translation_page(PAGE_TABLE_AT);
    assert!(
        !rig.cpu
            .jit_fast_map
            .dormant_has_write_mapping_for_test(PTE_ADDRESS)
    );
    set_cr3(&mut rig.cpu, &mut rig.bus, PAGE_DIRECTORY_AT);
    assert!(
        !rig.cpu
            .jit_fast_map
            .has_write_mapping(PTE_ADDRESS, PAGE_TABLE_AT)
    );
}

/// A native load from the page-table page still completes inside the block.
#[test]
fn a_native_load_from_a_translation_page_stays_native() {
    let config = Config {
        warm_read: true,
        ..Config::with_program(load_program())
    };
    let native = run_leg_with(true, &config);
    assert_eq!(native.exits_unavailable, 0, "{native:#?}");
    assert!(native.native_insns >= 2, "{native:#?}");
    assert_eq!(native.ecx, OLD_PTE, "{native:#?}");
    assert_eq!(native.translation_page_writes, 0, "{native:#?}");
}

/// The refusal counter counts one refused write fill for the warm-up store and one for the store
/// the interpreter runs after the block leaves.
#[test]
fn the_refusal_counter_counts_the_refused_write_fills() {
    let native = run_leg(true);
    assert_eq!(native.refused_warmup, 1, "{native:#?}");
    assert_eq!(native.refused_run, 1, "{native:#?}");
}
