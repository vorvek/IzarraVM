// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable disp32 lanes: `MOV r8, [disp32]` whose displacement is read out of guest RAM on
//! every execution instead of being baked into the emitted address arithmetic, so a guest patch
//! of those four bytes keeps the compiled block.
//!
//! The fixture is duke3d's shape (the 0x2AFxxx patch loops in
//! dev_docs/2026-08-08-dispatch-tier-next.md): `8a 1d dd dd dd dd`, `MOV BL, [disp32]`, ModRM
//! mod 0 reg 3 rm 5, six bytes, displacement at offset 2.
//!
//! The load sits at slot 1, never at the block's entry, for the reason the imm-lane fixture
//! states: an opcode at the entry position is not reached by the emitted body on this fixture
//! path, so an entry-position load would leave the lane emitter untested while every assertion
//! still passed.

use super::*;

/// Block entry. Chosen so the lane lands 4-byte aligned, the alignment a Build-engine patch
/// store actually has and the one that takes the FastMap write path.
const ENTRY: u32 = 0x500;
/// Offset of the `MOV BL, [disp32]` inside the block: after the two-byte `mov esi, esi`.
const LOAD_OFFSET: u32 = 2;
/// The lane: the load's displacement field, two bytes into the instruction.
const LANE: u32 = ENTRY + LOAD_OFFSET + 2;
/// The data bytes the patched displacements point at, all inside the 0x5000 image and all OFF
/// the code page: a read from the block's own (code-watched) page side-exits to the
/// interpreter, which is coherence behavior this fixture does not test.
const DATA: [(u32, u8); 4] = [
    (0x2000, 0xaa),
    (0x3000, 0xbb),
    (0x4443, 0xcc),
    (0x1000, 0x00),
];

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_fast_map_enabled_for_test(true);
    cpu.set_mode(GswMode::Gsw486);
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
    cpu.set_eip(ENTRY);
    cpu
}

/// `mov esi, esi` / `mov bl, [disp32]` / `mov edi, edi` / `hlt`. The HLT is a hard boundary, so
/// the block is exactly the three instructions before it.
fn image(disp: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x8a, 0x1d];
    code.extend_from_slice(&disp.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for (addr, value) in DATA {
        memory[addr as usize] = value;
    }
    memory
}

fn decode_at(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

fn block_starts() -> [u32; 3] {
    [ENTRY, ENTRY + LOAD_OFFSET, ENTRY + LOAD_OFFSET + 6]
}

fn install(cpu: &mut CpuGsw, entry: u32, instructions: u8) -> jit::direct::BlockId {
    let key = jit::direct::key_for(cpu, entry, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, entry, true).expect("fixture block compiles");
    assert_eq!(
        compilation.span.instructions, instructions,
        "fixture block shape changed"
    );
    // Mirrors the production install site in `run.rs`, both counters: the aggregate and the
    // disp split. A fixture that installed without them would read zero lanes for a
    // lane-bearing block.
    let lanes = compilation.imm_lane_count() as u64;
    let disp_lanes = compilation.disp_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    if disp_lanes != 0 {
        cpu.jit_direct
            .direct
            .note_disp_lane_registrations(disp_lanes);
    }
    id
}

fn arm(cpu: &mut CpuGsw, ebx: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ebx(ebx);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = 0x8d7;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(ENTRY);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn test_bus(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus
}

/// Identity-map the whole 0x5000 image into the fast map. A memory-bearing block compiles only
/// once `native_bases()` answers (the compile loop returns `Retry` otherwise), and the native
/// load needs the DATA pages resolvable at run time.
fn map_flat_pages(cpu: &mut CpuGsw, bus: &mut TestBus) {
    for page in (0..0x5000u32).step_by(0x1000) {
        let read = bus
            .direct_page(page, BusAccessKind::DataRead)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_read(
            page,
            page,
            read,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page)
        ));
        let write = bus
            .direct_page(page, BusAccessKind::DataWrite)
            .unwrap()
            .unwrap();
        assert!(cpu.jit_fast_map.populate_write(
            page,
            page,
            write,
            jit::fast_map::PagePermissions::UNPAGED,
            cpu.physical_page_watched(page)
        ));
    }
}

fn guest_store(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Dword,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

fn guest_store_word(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Word,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

/// Seed a heat RECORD (not hotness) for the four bytes at `lane`, the way one real patch of a
/// decoded instruction leaves one: `note_code_write_inner` bumps the written chunk on every
/// heat-charged kill. Admission (`disp_lane_for`) requires this measured patch history — a
/// never-patched load compiles baked, which is the doom-gate cut.
fn seed_patch_history(cpu: &mut CpuGsw, lane: u32) {
    cpu.sync_smc_heat();
    cpu.jit_direct.smc_heat.bump(lane, 4, 0);
}

/// Compile and install the lane block, and hand back everything a patch-then-run needs.
fn lane_fixture(disp: u32) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    // The knob is a process-global OnceLock with no test override; a shell that still exports
    // the A/B off-arm would otherwise fail five tests here opaquely.
    assert!(
        jit::direct::disp_lanes_enabled(),
        "IZARRAVM_DISP_LANES=0 is exported in this environment; unset it to run the lane tests"
    );
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(disp));
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &block_starts());
    seed_patch_history(&mut cpu, LANE);
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the fixture load did not take a lane; every assertion below would be vacuous"
    );
    assert_eq!(
        cpu.direct_stall_snapshot().disp_lane_registrations,
        1,
        "the lane must be counted as a DISP lane, not an imm lane"
    );
    (cpu, bus, id)
}

/// The emitter: a lane block's result must equal the interpreter's, including after the guest
/// patches the displacement between executions. The interpreter side runs the same bytes with
/// no block at all, so it re-decodes the patched instruction and is the reference by
/// construction.
#[test]
fn disp_lane_load_matches_the_interpreter_across_patches() {
    let patches = [DATA[0].0, DATA[1].0, DATA[2].0, DATA[3].0, DATA[1].0];
    let mut native = flat_cpu();
    let mut native_bus = test_bus(image(patches[0]));
    map_flat_pages(&mut native, &mut native_bus);
    decode_at(&mut native, &mut native_bus, &block_starts());
    seed_patch_history(&mut native, LANE);
    let id = install(&mut native, ENTRY, 3);

    let mut interpreter = flat_cpu();
    let mut interpreter_bus = test_bus(image(patches[0]));
    decode_at(&mut interpreter, &mut interpreter_bus, &block_starts());

    for (round, &disp) in patches.iter().enumerate() {
        if round != 0 {
            guest_store(&mut native, &mut native_bus, LANE, disp);
            guest_store(&mut interpreter, &mut interpreter_bus, LANE, disp);
        }
        // A varying non-zero seed, so the byte merge into EBX's low lane is visible against
        // whatever the previous round left there.
        let ebx = 0x1234_5600u32.wrapping_add(round as u32 + 1);
        arm(&mut native, ebx);
        arm(&mut interpreter, ebx);

        let block = native
            .jit_direct
            .block(id)
            .expect("the lane block survives every patch");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "native block did not run in round {round}"
        );
        for _ in 0..3 {
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }

        assert_eq!(
            crate::tests::settled_registers(&native),
            crate::tests::settled_registers(&interpreter),
            "registers differ after patch {disp:#010x}"
        );
        assert_eq!(
            native.eflags(),
            interpreter.eflags(),
            "EFLAGS differ after patch {disp:#010x}"
        );
        let expected = native_bus.memory[disp as usize];
        assert_eq!(
            native.registers.ebx(),
            (ebx & 0xffff_ff00) | u32::from(expected),
            "the native load did not read through the CURRENT displacement {disp:#010x}"
        );
        assert_eq!(
            native_bus.memory, interpreter_bus.memory,
            "guest memory differs after patch {disp:#010x}"
        );
    }
}

/// The accept case: exactly four bytes at exactly the lane start. The block stays installed,
/// the next entry is still native, and that entry reads through the NEW displacement.
#[test]
fn disp_lane_write_preserves_the_owning_block_and_its_native_entry() {
    let (mut cpu, mut bus, id) = lane_fixture(DATA[0].0);
    let before_kills = cpu.perf_counters().smc_narrow_kills;
    guest_store(&mut cpu, &mut bus, LANE, DATA[1].0);

    let after = cpu.perf_counters();
    assert_eq!(after.smc_lane_accepts, 1);
    assert_eq!(after.smc_lane_reject_width, 0);
    assert_eq!(after.smc_lane_reject_address, 0);
    assert!(
        after.smc_narrow_kills > before_kills,
        "the interpreter's decode line must still be killed"
    );

    let block = cpu
        .jit_direct
        .block(id)
        .expect("a lane write must not retire the owning block");
    arm(&mut cpu, 0x100);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(cpu.registers.ebx(), 0x100 | u32::from(DATA[1].1));
}

/// A lane write is not code churn: enough patches to cross the heat threshold several times
/// over must leave the heat map untouched, because that demotion pressure is what the disp lane
/// exists to remove (dormant_heat was 44.8% of duke3d-586's dispatcher seams).
#[test]
fn disp_lane_writes_contribute_no_smc_heat() {
    let (mut cpu, mut bus, id) = lane_fixture(DATA[0].0);
    for round in 0..64u32 {
        // Every value distinct from the last and none equal to the compiled displacement, so
        // G2 same-value elision never hides a round from the choke.
        guest_store(&mut cpu, &mut bus, LANE, 0x1000 + round);
    }
    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 64);
    assert_eq!(
        perf.smc_heat_chunks_hot, 0,
        "lane patches must not heat the chunk"
    );
    assert_eq!(perf.smc_heat_demotions, 0);
    assert!(
        cpu.jit_direct.block(id).is_some(),
        "the block must survive all of them"
    );
    let _ = &mut bus;
}

/// The width check, fail-closed: a two-byte patch of the dword field starts at the lane but is
/// not the admitted shape, so it takes the normal invalidation path.
///
/// This pins the shared choke term (`width == IMM_LANE_WIDTH`) FOR A DISP-FIELD LANE — it is
/// deliberate duplicate coverage of logic the imm-lane suite already pins, not this slice's
/// mutation record. The slice's own records are the emit seam (baking the displacement back in
/// fails the two read-through tests) and the admission fixtures (`a_disp8_form_takes_no_lane`
/// for `disp_len == 4`, `a_sib_disp32_form_lanes_at_the_right_offset` for the `len - 4` start,
/// `a_cold_chunk_load_takes_no_lane` for the heat gate).
#[test]
fn word_write_at_the_disp_lane_start_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(DATA[0].0);
    guest_store_word(&mut cpu, &mut bus, LANE, 0x1234);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 1);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a partial patch of the displacement must retire the block"
    );
}

/// A four-byte write that overlaps the lane but starts one byte early — the ModRM byte plus
/// three displacement bytes. The resulting instruction is not the one that compiled.
#[test]
fn straddling_write_over_the_disp_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(DATA[0].0);
    guest_store(&mut cpu, &mut bus, LANE - 1, 0x1122_3344);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 1);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a straddling write must retire the block"
    );
}

/// A disp8 form is refused, and the fixture is chosen to pin the `disp_len == 4` term rather
/// than ride on arithmetic accident: `8a 5c 24 10` is `MOV BL, [ESP+0x10]` (mod 01, SIB), a
/// FOUR-byte instruction, so with the term deleted `len - 4` is 0 and the lane would land on
/// the OPCODE byte — a 4-byte guest write rewriting the instruction itself would then be
/// absorbed as a lane accept while the block keeps running the old code. A 3-byte disp8 form
/// (the first version of this fixture) never reaches that state, because `checked_sub(4)`
/// underflows and refuses for the wrong reason; the adversarial review's mutation run proved
/// the term untested that way.
#[test]
fn a_disp8_form_takes_no_lane() {
    let mut cpu = flat_cpu();
    let mut memory = vec![0u8; 0x5000];
    let code = [0x89, 0xf6, 0x8a, 0x5c, 0x24, 0x10, 0x89, 0xff, 0xf4];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2, ENTRY + 6]);
    // Heat seeded where the len-4 arithmetic WOULD put the lane (the opcode byte), so the
    // refusal below is the disp_len term's doing, not the heat gate's.
    seed_patch_history(&mut cpu, ENTRY + LOAD_OFFSET);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the block compiles");
    assert_eq!(compilation.span.instructions, 3, "fixture shape changed");
    assert_eq!(compilation.imm_lane_count(), 0, "no disp32 field, no lane");
    assert_eq!(compilation.disp_lane_count(), 0);
}

/// The SIB disp32 shape (`8a 1c 85 dd dd dd dd`, `MOV BL, [EAX*4 + disp32]`, SEVEN bytes with
/// the displacement at offset 3) — the one admitted encoding whose displacement is NOT two
/// bytes into the instruction. It pins the `len - 4` lane-start arithmetic: the review's
/// mutation run replaced it with a fixed `+2` the six-byte fixture cannot distinguish, and
/// every other test stayed green while the emitted code read its address out of the ModRM and
/// SIB bytes. EAX is zero on entry (`arm` clears the file), so the load resolves to the bare
/// displacement and the patched-read assertion is the same one the mod-0 fixture makes.
///
/// Indexed forms are IN deliberately (iteration 2 cut them and duke3d's whole win vanished —
/// Build patches these too); what keeps doom's never-patched `[base+disp32]` renderer loads
/// untaxed is the heat gate, pinned by `a_cold_chunk_load_takes_no_lane` below.
#[test]
fn a_sib_disp32_form_lanes_at_the_right_offset() {
    let mut cpu = flat_cpu();
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x8a, 0x1c, 0x85];
    code.extend_from_slice(&DATA[0].0.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for (addr, value) in DATA {
        memory[addr as usize] = value;
    }
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2, ENTRY + 9]);
    // The lane sits at instruction start + 3, one past where the six-byte form puts it.
    let sib_lane = ENTRY + LOAD_OFFSET + 3;
    seed_patch_history(&mut cpu, sib_lane);
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.direct_stall_snapshot().disp_lane_registrations,
        1,
        "the SIB form must take a lane"
    );

    guest_store(&mut cpu, &mut bus, sib_lane, DATA[1].0);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 1);

    let block = cpu
        .jit_direct
        .block(id)
        .expect("the lane write must not retire the SIB block");
    arm(&mut cpu, 0x300);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(
        cpu.registers.ebx(),
        0x300 | u32::from(DATA[1].1),
        "the native load must read through the patched SIB displacement"
    );
}

/// The heat gate, pinned from the refusing side: the SAME instruction that takes a lane in
/// every fixture above compiles BAKED when its displacement bytes carry no heat record. This
/// is the doom-gate cut made testable — iteration 1 laned every disp32 load unconditionally
/// and the formal gate failed on doom's never-patched renderer loads (paired RTF 0.978/0.975).
/// Deleting the `has_record_range` term in `disp_lane_for` makes this fail on the
/// registration counter.
#[test]
fn a_cold_chunk_load_takes_no_lane() {
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(DATA[0].0));
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &block_starts());
    // No seed_patch_history: the fixture differs from lane_fixture in exactly that line.

    // The probe is what tracks the key (install refuses an untracked one).
    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the block compiles");
    assert_eq!(compilation.span.instructions, 3, "fixture shape changed");
    assert_eq!(
        compilation.disp_lane_count(),
        0,
        "a never-patched displacement must compile baked (the doom-gate cut)"
    );

    // And with no lane, the patch a lane would have absorbed retires the block instead — after
    // which the RECOMPILE picks the lane up, because the kill is exactly what writes the heat
    // record admission reads. That convergence is the mechanism's whole story in one fixture.
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("the baked block installs");
    guest_store(&mut cpu, &mut bus, LANE, DATA[1].0);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "the first patch must retire the baked block"
    );
    decode_at(&mut cpu, &mut bus, &block_starts());
    let recompiled =
        jit::direct::compile(&mut cpu, ENTRY, true).expect("the block recompiles after the kill");
    assert_eq!(
        recompiled.disp_lane_count(),
        1,
        "the recompile after the first patch must take the lane"
    );
}

/// A prefixed form is refused, the same conservative bar `imm_lane_for` sets: the admitted
/// shape is byte-for-byte the one the census measured. `3e 8a 1d ..` carries a DS override that
/// changes nothing about the address, and it still gets the baked form.
#[test]
fn a_prefixed_form_takes_no_lane() {
    let mut cpu = flat_cpu();
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x3e, 0x8a, 0x1d];
    code.extend_from_slice(&DATA[0].0.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for (addr, value) in DATA {
        memory[addr as usize] = value;
    }
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &[ENTRY, ENTRY + 2, ENTRY + 9]);
    // Heat seeded on the prefixed form's disp bytes (instruction start + 3), so the refusal
    // is the prefix term's doing, not the heat gate's.
    seed_patch_history(&mut cpu, ENTRY + LOAD_OFFSET + 3);

    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the block compiles");
    assert_eq!(compilation.span.instructions, 3, "fixture shape changed");
    assert_eq!(compilation.disp_lane_count(), 0, "prefixes bar the lane");
}

/// The `IZARRAVM_DISP_LANES` spelling table, and THE DEFAULT PIN, TWO-SIDED.
///
/// `disp_lanes_enabled` caches its env reading in a process-wide `OnceLock`, so the contract is
/// otherwise assertable exactly once per process and never in an order the harness controls --
/// hence the parse function is exercised directly.
///
/// THE BUG THIS PINS. Until the lane-cap fix this knob read `!= "0"` with no table at all, so
/// `IZARRAVM_DISP_LANES=off` selected ON: a ladder leg spelling the escape the way the other three
/// lane knobs accept it ran the DEFAULT and reported the arm as inert. Restoring the bare form
/// fails the `off` entries below.
///
/// The EMPTY STRING is OFF while unset is ON, which is a deliberate change for this knob and the
/// convention every other lane knob already follows: nulling a variable in PowerShell leaves it
/// present and empty, and reading that as ON is how three earlier evidence directories came to run
/// their default-ON knobs on the wrong arm.
#[test]
fn disp_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_disp_lanes_arm_for_test;
    assert!(
        parse(Err(VarError::NotPresent)),
        "unset must select ON -- the shipped default"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(
            !parse(Ok(off.to_string())),
            "{off:?} must select the baked-displacement world; it is the escape and the A/B base"
        );
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(
            parse(Ok(on.to_string())),
            "{on:?} must select the lane class"
        );
    }
}

/// THE SHIPPED DEFAULT, asserted through the live reader rather than the parse table, for
/// `the_shipped_count_lanes_default_is_the_on_arm`'s reason: nothing else here would notice
/// `disp_lanes_enabled` growing a different default from the one `parse_disp_lanes_arm` spells.
///
/// Reads the AMBIENT arm, with the thread-local override explicitly cleared first.
#[test]
fn the_shipped_disp_lanes_default_is_the_on_arm() {
    jit::direct::set_disp_lanes_for_test(None);
    assert!(
        jit::direct::disp_lanes_enabled(),
        "the shipped default must be ON; the heat gate, not this knob, is what keeps doom's \
         never-patched loads baked"
    );
}

/// A typo must not silently run the default. See `parse_disp_lanes_arm` for why guessing is worse
/// than failing: a leg that quietly fell through would run exactly what an unset environment runs,
/// and be read as the arm it named doing nothing.
#[test]
#[should_panic(expected = "names no arm")]
fn an_unrecognised_disp_lanes_spelling_panics() {
    jit::direct::parse_disp_lanes_arm_for_test(Ok("true".to_string()));
}
