// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Option D: mutable disp32 lanes for `0x89 MOV r/m32, r32`, `0x88 MOV r/m8, r8`
//! (`IZARRAVM_DISP_STORE_LANES`) and `0x8B MOV r32, r/m32` (`IZARRAVM_DISP_LOAD_WIDEN`).
//!
//! **The two arms no longer share a default.** `IZARRAVM_DISP_STORE_LANES` is ON since the
//! 2026-08-23 ladder (`duke3d-586` long −25.0%); `IZARRAVM_DISP_LOAD_WIDEN` is still OFF, blocked
//! on the lane-cap re-price. Every fixture here forces the arm it means, per the STATE-THE-ARM
//! rule, and that is now load-bearing in BOTH directions: a fixture reading the ambient store
//! knob would silently follow the flip, and one reading the ambient widen knob would be vacuous
//! on a shipped binary. The two default PINS are the only fixtures that read the ambient value,
//! and they are what catch a default moving without a ladder.
//!
//! The fixture frame is `cpu_jit_disp_lane_test.rs`'s, one kind over: `mov esi, esi` /
//! *the subject* / `mov edi, edi` / `hlt`, with the subject at slot 1 rather than at the entry,
//! because an entry-position slot is not reached by the emitted body on this fixture path and an
//! entry-position subject would leave the lane emitter untested while every assertion passed.
//!
//! # What each group holds
//!
//! * **State identity.** A laned store must land the same bytes at the same guest address as the
//!   interpreter running the same code, across a sequence of guest patches of its displacement,
//!   at BOTH widths, and the `0x8B` load must read through its patched displacement the same way.
//!   The interpreter side runs with no block at all, so it re-decodes the patched instruction and
//!   is the reference by construction.
//! * **The choke.** A four-byte patch exactly at the lane is absorbed (`lane_only`), charges no
//!   heat and does not retire the block, and the next native entry uses the NEW displacement.
//! * **The cap, and where it sits.** The thirteenth laneable slot charges exactly one refusal, to
//!   the store family and to no other; and a slot the heat gate refuses charges NOTHING even with
//!   the budget already spent, which is what pins the cap BELOW the heat gate.
//! * **The off arm.** Emitted code identical for every shape the arm does not admit, different
//!   for the one it does, and every counter identical.
//! * **The knobs.** Spelling tables and the default-OFF pins.
//!
//! # Mutation record
//!
//! Recorded in `dev_docs/2026-08-23-option-d-step-3-log.md` alongside the results, so the log and
//! the record cannot drift apart.

use super::*;

/// Block entry, chosen so the lane lands 4-byte aligned — the alignment a real Build-engine patch
/// store has, and the one that takes the FastMap write path.
const ENTRY: u32 = 0x500;
/// Offset of the subject inside the block: after the two-byte `mov esi, esi`.
const SUBJECT_OFFSET: u32 = 2;
/// The lane: the subject's displacement field, two bytes into a six-byte mod-0 rm-5 form.
const LANE: u32 = ENTRY + SUBJECT_OFFSET + 2;

/// The addresses the patched displacements name. All inside the 0x5000 image, all OFF the code
/// page (a store into the block's own watched page side-exits, which is coherence behaviour this
/// file does not test), and all 4-byte aligned so the dword fixtures never meet the misalignment
/// side exit.
const TARGETS: [u32; 4] = [0x2000, 0x3000, 0x4440, 0x1000];

/// `mov ebx, [disp32]`'s data, one distinguishable dword per target.
const SEEDS: [u32; 4] = [0xa1a2_a3a4, 0xb1b2_b3b4, 0xc1c2_c3c4, 0xd1d2_d3d4];

/// `mov [disp32], ebx`, mod 0 reg 3 (EBX) rm 5. Six bytes, disp32 at offset 2.
const STORE_DWORD: [u8; 2] = [0x89, 0x1d];
/// `mov [disp32], bl`. Six bytes, disp32 at offset 2.
const STORE_BYTE: [u8; 2] = [0x88, 0x1d];
/// `mov ebx, [disp32]`. Six bytes, disp32 at offset 2.
const LOAD_DWORD: [u8; 2] = [0x8b, 0x1d];

/// `add ebp, imm32`, the `imm_lane_for` filler the cap fixtures spend the budget with. Six bytes,
/// immediate at offset 2. Admitted on BOTH arms of `IZARRAVM_LANE_FAMILY` (`/0 ADD` at Dword is
/// the narrow arm's whole admission set), so no cap fixture here depends on a knob it forces.
const IMM_SLOT: [u8; 6] = [0x81, 0xc5, 0x11, 0x22, 0x33, 0x44];

/// The shared budget, spelled here so a fixture that stops matching the constant fails loudly
/// rather than silently testing a block that never reaches the cap.
const LANES: usize = jit::direct::MAX_BLOCK_IMM_LANES;

type CapRefusals = [u64; jit::direct::LANE_CAP_FAMILIES];

/// Every arm this file can force, restored on drop. `Drop` rather than trailing statements
/// because an assertion failure is the normal way a fixture ends when something is wrong, and a
/// panic skips trailing statements — the thread is reused by the harness and a leaked override
/// would corrupt an unrelated test.
struct ArmOverride {
    /// The STORE-PATH arm, which is not an environment knob but a per-`JitState` field
    /// (`IZARRAVM_ONE_LOOKUP_STORE` only seeds its default). It rides on this struct rather than
    /// being set ad hoc so that `flat_cpu` CANNOT build a CPU without a fixture having stated it:
    /// the two store emitters are different code (`emit_store` classifies the page kind itself,
    /// `emit_store_fast` goes through the bias table), they both consume the lane through
    /// `emit_segmented_linear_address`, and a fixture that only ever compiled one of them would
    /// leave half the Option D store admission untested.
    one_lookup_store: bool,
}

impl Drop for ArmOverride {
    fn drop(&mut self) {
        jit::direct::set_lane_family_for_test(None);
        jit::direct::set_imm8_lanes_for_test(None);
        jit::direct::set_count_lanes_for_test(None);
        jit::direct::set_disp_lanes_for_test(None);
        jit::direct::set_disp_store_lanes_for_test(None);
        jit::direct::set_disp_load_widen_for_test(None);
    }
}

/// State all six lane arms. Bind the result for the fixture's lifetime.
///
/// The four shipped arms are pinned at their shipped defaults rather than left ambient, so a
/// fixture here reads the same on a suite run with `IZARRAVM_IMM8_LANES=0` exported as it does
/// without it, and so the four leading zeros in a `CapRefusals` assertion mean "that family had
/// no slot" rather than "that family's knob happened to be off".
#[must_use]
fn force_arms(store: bool, load_widen: bool, one_lookup_store: bool) -> ArmOverride {
    jit::direct::set_lane_family_for_test(Some(true));
    jit::direct::set_imm8_lanes_for_test(Some(true));
    jit::direct::set_count_lanes_for_test(Some(true));
    jit::direct::set_disp_lanes_for_test(Some(true));
    jit::direct::set_disp_store_lanes_for_test(Some(store));
    jit::direct::set_disp_load_widen_for_test(Some(load_widen));
    ArmOverride { one_lookup_store }
}

/// The store path the fixtures that are not about the store path itself run on: the SHIPPED
/// default (`one_lookup_store` seeds ON from `IZARRAVM_ONE_LOOKUP_STORE`). Named rather than
/// spelled `true` at twelve call sites so a reader can see at a glance which fixtures state an
/// arm and which take the shipped one.
const SHIPPED_STORE_PATH: bool = true;

fn flat_cpu(arms: &ArmOverride) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    // Pinned rather than left at `one_lookup_store_default()`, so a suite run with
    // `IZARRAVM_ONE_LOOKUP_STORE=0` exported reads the same as one without it.
    cpu.jit_direct.one_lookup_store = arms.one_lookup_store;
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

fn test_bus(memory: Vec<u8>) -> TestBus {
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    bus
}

/// The frame with `subject` (a two-byte opcode+ModRM prefix) pointed at `disp`, and every target
/// pre-seeded so a load fixture has something distinguishable to read.
fn image(subject: [u8; 2], disp: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6];
    code.extend_from_slice(&subject);
    code.extend_from_slice(&disp.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    for (target, seed) in TARGETS.iter().zip(SEEDS) {
        memory[*target as usize..*target as usize + 4].copy_from_slice(&seed.to_le_bytes());
    }
    memory
}

/// Identity-map the whole image. A memory-bearing block compiles only once `native_bases()`
/// answers, and the native store needs its target page resolvable at run time.
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
    [ENTRY, ENTRY + SUBJECT_OFFSET, ENTRY + SUBJECT_OFFSET + 6]
}

/// Seed a heat RECORD (not hotness) for the four bytes at `lane`, the way one real patch of a
/// decoded instruction leaves one. Both arms demand this measured patch history — a never-patched
/// slot compiles baked, which is the doom cut, inherited verbatim from `disp_lane_for`.
fn seed_patch_history(cpu: &mut CpuGsw, lane: u32) {
    cpu.sync_smc_heat();
    cpu.jit_direct.smc_heat.bump(lane, 4, 0);
}

/// Compile and install, mirroring the production install site in `run.rs` for ALL five
/// registration counters. A fixture that installed without them would read zero lanes for a
/// lane-bearing block.
fn install(cpu: &mut CpuGsw, instructions: u8) -> jit::direct::BlockId {
    let key = jit::direct::key_for(cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(cpu, ENTRY, true).expect("fixture block compiles");
    assert_eq!(
        compilation.span.instructions, instructions,
        "fixture block shape changed"
    );
    let lanes = compilation.imm_lane_count() as u64;
    let store = compilation.disp_store_lane_count() as u64;
    let widen = compilation.disp_load_widen_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    if store != 0 {
        cpu.jit_direct
            .direct
            .note_disp_store_lane_registrations(store);
    }
    if widen != 0 {
        cpu.jit_direct
            .direct
            .note_disp_load_widen_lane_registrations(widen);
    }
    id
}

fn arm_cpu(cpu: &mut CpuGsw, ebx: u32) {
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

/// A guest patch of four bytes at a LINEAR address.
///
/// Through ES, not DS, and that is load-bearing rather than arbitrary: the finite-segment fixture
/// gives DS a non-zero base, so a DS-relative patch of `LANE` would land 0x1000 bytes away and
/// silently never patch the lane at all. ES stays flat in every fixture here, so `linear` means
/// linear at every call site.
fn guest_store(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Es,
        linear,
        BusWidth::Dword,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

fn cap_refusals(cpu: &CpuGsw) -> CapRefusals {
    let stalls = cpu.direct_stall_snapshot();
    [
        stalls.imm_lane_cap_refusals,
        stalls.imm8_lane_cap_refusals,
        stalls.count_lane_cap_refusals,
        stalls.disp_lane_cap_refusals,
        stalls.disp_store_lane_cap_refusals,
        stalls.disp_load_widen_lane_cap_refusals,
    ]
}

/// Compile and install the frame with `subject`, and hand back everything a patch-then-run needs.
fn lane_fixture(
    arms: &ArmOverride,
    subject: [u8; 2],
    disp: u32,
) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    let mut cpu = flat_cpu(arms);
    let mut bus = test_bus(image(subject, disp));
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &block_starts());
    seed_patch_history(&mut cpu, LANE);
    let id = install(&mut cpu, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the subject did not take a lane; every assertion below would be vacuous"
    );
    (cpu, bus, id)
}

/// THE STATE-IDENTITY FIXTURE, parameterised over the three admitted opcodes.
///
/// Runs the same code twice — once as an installed native block, once with no block at all — and
/// compares the FULL architectural state (registers, EFLAGS, lazy flags, all of guest memory)
/// after every round, patching the displacement between rounds. The interpreter re-decodes the
/// patched instruction, so it is the reference by construction, and comparing whole memory rather
/// than one address is what makes a store that landed at the OLD displacement a failure.
fn state_identity(subject: [u8; 2], store: bool, widen: bool, one_lookup_store: bool) {
    let arms = force_arms(store, widen, one_lookup_store);
    let patches = [TARGETS[0], TARGETS[1], TARGETS[2], TARGETS[3], TARGETS[1]];

    let mut native = flat_cpu(&arms);
    let mut native_bus = test_bus(image(subject, patches[0]));
    map_flat_pages(&mut native, &mut native_bus);
    decode_at(&mut native, &mut native_bus, &block_starts());
    seed_patch_history(&mut native, LANE);
    let id = install(&mut native, 3);
    assert_eq!(
        native.perf_counters().smc_lane_registrations,
        1,
        "{subject:02x?} (one_lookup_store={one_lookup_store}) did not take a lane; the \n             comparison below would be vacuous"
    );

    let mut interpreter = flat_cpu(&arms);
    let mut interpreter_bus = test_bus(image(subject, patches[0]));
    decode_at(&mut interpreter, &mut interpreter_bus, &block_starts());

    for (round, &disp) in patches.iter().enumerate() {
        if round != 0 {
            guest_store(&mut native, &mut native_bus, LANE, disp);
            guest_store(&mut interpreter, &mut interpreter_bus, LANE, disp);
        }
        // A varying non-zero value, so a store that wrote the previous round's bytes is visible
        // and so the byte merge into EBX's low lane is visible on the load arm.
        let ebx = 0x1234_5600u32.wrapping_add(round as u32 + 1);
        arm_cpu(&mut native, ebx);
        arm_cpu(&mut interpreter, ebx);

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
            native.registers, interpreter.registers,
            "registers differ after patch {disp:#010x} (one_lookup_store={one_lookup_store})"
        );
        assert_eq!(
            native.eflags(),
            interpreter.eflags(),
            "EFLAGS differ after patch {disp:#010x} (one_lookup_store={one_lookup_store})"
        );
        assert_eq!(
            native_bus.memory, interpreter_bus.memory,
            "guest memory differs after patch {disp:#010x} (one_lookup_store={one_lookup_store})"
        );
    }
}

/// `0x89 MOV [disp32], r32` — the prize. 89% of the un-laned displacement mass by events and
/// 96.8% of its joined block kills.
#[test]
fn dword_store_lane_matches_the_interpreter_across_patches() {
    for one_lookup_store in [true, false] {
        state_identity(STORE_DWORD, true, false, one_lookup_store);
    }
}

/// `0x88 MOV [disp32], r8` — the same instruction one width down, and the second width the
/// admission covers. It shares `emit_store` with `0x89`, so a width-dependent regression in the
/// lane arm shows here and not above.
#[test]
fn byte_store_lane_matches_the_interpreter_across_patches() {
    for one_lookup_store in [true, false] {
        state_identity(STORE_BYTE, true, false, one_lookup_store);
    }
}

/// `0x8B MOV r32, [disp32]` — the load widening, which is a different KIND and a different
/// emitter from the two above and is separately knobbed.
#[test]
fn dword_load_widen_lane_matches_the_interpreter_across_patches() {
    // The load arm too, and its second leg is NOT vacuous: `one_lookup_store` gates the store
    // STUB PAD, which `compile` builds for any store-bearing block, so a widened load compiled
    // beside stores would meet a different emission budget under the two arms.
    for one_lookup_store in [true, false] {
        state_identity(LOAD_DWORD, false, true, one_lookup_store);
    }
}

/// THE MID-BLOCK PATCH: the store lane's own record is rewritten while the block is installed.
///
/// The write is absorbed (`lane_only`), so the block is NOT retired, the heat map is untouched,
/// and the very next native entry stores through the NEW displacement. That last clause is the
/// one an emitter that baked the displacement back in would fail while every counter still read
/// correctly.
#[test]
fn a_store_lane_write_preserves_the_owning_block_and_its_native_entry() {
    let arms = force_arms(true, false, SHIPPED_STORE_PATH);
    let (mut cpu, mut bus, id) = lane_fixture(&arms, STORE_DWORD, TARGETS[0]);
    assert_eq!(
        cpu.direct_stall_snapshot().disp_store_lane_registrations,
        1,
        "the lane must be counted as a STORE lane, not a disp or imm lane"
    );
    assert_eq!(cpu.direct_stall_snapshot().disp_lane_registrations, 0);

    let before_kills = cpu.perf_counters().smc_narrow_kills;
    guest_store(&mut cpu, &mut bus, LANE, TARGETS[1]);

    let after = cpu.perf_counters();
    assert_eq!(after.smc_lane_accepts, 1);
    assert_eq!(after.smc_lane_reject_width, 0);
    assert_eq!(after.smc_lane_reject_address, 0);
    assert!(
        after.smc_narrow_kills > before_kills,
        "the interpreter's decode line must still be killed"
    );
    assert_eq!(
        after.smc_heat_chunks_hot, 0,
        "an absorbed patch must charge no heat"
    );

    let block = cpu
        .jit_direct
        .block(id)
        .expect("a lane write must not retire the owning block");
    arm_cpu(&mut cpu, 0x5150_5152);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(
        &bus.memory[TARGETS[1] as usize..TARGETS[1] as usize + 4],
        &0x5150_5152u32.to_le_bytes(),
        "the native store must land at the PATCHED displacement"
    );
    assert_eq!(
        &bus.memory[TARGETS[0] as usize..TARGETS[0] as usize + 4],
        &SEEDS[0].to_le_bytes(),
        "nothing may land at the displacement the block compiled with"
    );
}

/// A laned store patch is not code churn: enough patches to cross the heat threshold several
/// times over must leave the heat map untouched, because that demotion pressure is the whole
/// prize (the 2026-08-23 settling census attributes 21,178 of 21,882 joined un-laned-disp block
/// kills on duke3d-586-short to `0x89` alone).
#[test]
fn store_lane_writes_contribute_no_smc_heat() {
    let arms = force_arms(true, false, SHIPPED_STORE_PATH);
    let (mut cpu, mut bus, id) = lane_fixture(&arms, STORE_DWORD, TARGETS[0]);
    for round in 0..64u32 {
        // Every value distinct from the last and none equal to the compiled displacement, so G2
        // same-value elision never hides a round from the choke. Page 1 throughout, which is
        // mapped and is not the code page.
        guest_store(&mut cpu, &mut bus, LANE, 0x1000 + round * 4);
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
}

/// A block of `fillers` `IMM_SLOT`s followed by `tail`, plus a stopper. Returns the image and the
/// linear start of every instruction the walk will slot.
///
/// The trailing `0xF4` is a STOPPER, not a slot: `decode_at` primes exactly the starts returned
/// here, so the walk meets a decode miss there and ends with `CompileStop::Retry(DecodeMiss)`,
/// which is a clean end for a block that already carries twelve or thirteen slots.
fn capped_image(fillers: usize, tail: &[u8]) -> (Vec<u8>, Vec<u32>) {
    let mut code = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..fillers {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(&IMM_SLOT);
    }
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(tail);
    code.push(0xf4);
    let mut memory = vec![0u8; 0x5000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    (memory, starts)
}

/// Compile (and install) a `fillers` + `tail` block, optionally seeding the tail's disp record.
/// The bus is returned so it outlives the compilation: the lanes hold host pointers into it.
fn capped_fixture(
    arms: &ArmOverride,
    fillers: usize,
    tail: &[u8],
    seed_tail: bool,
) -> (CpuGsw, TestBus, jit::direct::Compilation) {
    let (memory, starts) = capped_image(fillers, tail);
    let mut cpu = flat_cpu(arms);
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(&mut cpu, &mut bus, &starts);
    if seed_tail {
        let tail_start = *starts.last().expect("the fixture has a tail slot");
        seed_patch_history(&mut cpu, tail_start + 2);
    }
    assert_eq!(
        cap_refusals(&cpu),
        [0; jit::direct::LANE_CAP_FAMILIES],
        "the fixture charged a cap refusal before it compiled anything"
    );
    // `install` only accepts a key the cache has already SEEN, so the fixture walks the
    // production order: probe, compile, install.
    let key = jit::direct::key_for(&cpu, ENTRY, true).expect("the fixture entry has a block key");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = jit::direct::compile(&mut cpu, ENTRY, true).expect("the fixture compiles");
    assert_eq!(
        compilation.span.instructions as usize,
        starts.len(),
        "the fixture block did not slot every instruction"
    );
    cpu.jit_direct
        .install(&compilation)
        .expect("the fixture block installs");
    (cpu, bus, compilation)
}

/// THE CAP, charged to the store family and to nobody else.
///
/// Twelve register-only `ADD EBP, imm32` fillers spend the budget; the thirteenth slot is the
/// `0x89` store. Asserting all SIX counters rather than one is the point: a cap test hoisted
/// above the store arm's kind and opcode bars would move more than one of them on this slot, and
/// the five zeros are what catches it.
#[test]
fn the_thirteenth_store_slot_charges_exactly_one_store_cap_refusal() {
    let arms = force_arms(true, false, SHIPPED_STORE_PATH);
    let mut tail = STORE_DWORD.to_vec();
    tail.extend_from_slice(&TARGETS[0].to_le_bytes());
    let (cpu, _bus, compilation) = capped_fixture(&arms, LANES, &tail, true);
    assert_eq!(
        compilation.imm_lane_count(),
        LANES,
        "the fillers must spend the whole budget"
    );
    assert_eq!(
        compilation.disp_store_lane_count(),
        0,
        "the capped slot must not have taken a lane"
    );
    assert_eq!(cap_refusals(&cpu), [0, 0, 0, 0, 1, 0]);
}

/// THE CAP SITS BELOW THE HEAT GATE, stated as a counter reading rather than as an argument.
///
/// Same twelve fillers, same thirteenth `0x89` slot, and the ONLY difference is that its
/// displacement carries no patch history. The heat gate refuses it first, so nothing is charged
/// — not even to its own family. Invert the two and this fixture reads `[0, 0, 0, 0, 1, 0]`,
/// which is the whole `0x89` population being reported as budget pressure on any real workload.
#[test]
fn a_store_slot_the_heat_gate_refuses_charges_no_cap_refusal() {
    let arms = force_arms(true, false, SHIPPED_STORE_PATH);
    let mut tail = STORE_DWORD.to_vec();
    tail.extend_from_slice(&TARGETS[0].to_le_bytes());
    let (cpu, _bus, compilation) = capped_fixture(&arms, LANES, &tail, false);
    assert_eq!(compilation.imm_lane_count(), LANES);
    assert_eq!(compilation.disp_store_lane_count(), 0);
    assert_eq!(cap_refusals(&cpu), [0; jit::direct::LANE_CAP_FAMILIES]);
}

/// The same pair for the load-widening arm, so neither arm can inherit the other's cap cell.
#[test]
fn the_thirteenth_load_widen_slot_charges_exactly_one_widen_cap_refusal() {
    let arms = force_arms(false, true, SHIPPED_STORE_PATH);
    let mut tail = LOAD_DWORD.to_vec();
    tail.extend_from_slice(&TARGETS[0].to_le_bytes());
    let (cpu, _bus, compilation) = capped_fixture(&arms, LANES, &tail, true);
    assert_eq!(compilation.imm_lane_count(), LANES);
    assert_eq!(compilation.disp_load_widen_lane_count(), 0);
    assert_eq!(cap_refusals(&cpu), [0, 0, 0, 0, 0, 1]);
}

/// THE OFF ARM CHARGES NOTHING, which is what separates a lane-BUDGET reading from a lane-CLASS
/// one on the ladder legs these counters exist for.
///
/// The knob is tested above the budget in both arms, so a capped thirteenth slot of either shape
/// reads zero everywhere with its arm off. Fusing the knob into the cap's disjunction — the shape
/// three matchers used to have — makes both of these read a refusal.
#[test]
fn the_off_arms_charge_no_cap_refusal_at_the_thirteenth_slot() {
    for subject in [STORE_DWORD, LOAD_DWORD] {
        let arms = force_arms(false, false, SHIPPED_STORE_PATH);
        let mut tail = subject.to_vec();
        tail.extend_from_slice(&TARGETS[0].to_le_bytes());
        let (cpu, _bus, compilation) = capped_fixture(&arms, LANES, &tail, true);
        assert_eq!(compilation.disp_store_lane_count(), 0);
        assert_eq!(compilation.disp_load_widen_lane_count(), 0);
        assert_eq!(
            cap_refusals(&cpu),
            [0; jit::direct::LANE_CAP_FAMILIES],
            "{subject:02x?} charged a cap refusal on its OFF arm"
        );
    }
}

/// Compile the frame with `middle` under a forced pair of arms and hand back the emitted code's
/// LENGTH.
///
/// The length rather than the bytes, and that is a limitation of the fixture rather than a
/// weakening of the claim: two compilations bake different host pointers (link-cell and portal
/// addresses, and a lane's own host pointer), so a byte comparison across them fails on
/// allocation noise whatever the arm. The length is stable, and it is exactly the quantity a
/// stray lane moves — the lane arm emits `mov r64, imm64` plus `mov eax, [r64]` where the baked
/// arm emits one `mov eax, imm32`.
fn emitted_len_under_arms(middle: &[u8], store: bool, widen: bool) -> usize {
    emitted_len_on_store_path(middle, store, widen, SHIPPED_STORE_PATH, true)
}

/// The same, with NO heat record seeded — the state main was always in for a store, because main
/// had no store matcher to consult a record with. Used only by the "the OFF arm is main" claim.
fn emitted_len_unseeded(middle: &[u8], store: bool, widen: bool) -> usize {
    emitted_len_on_store_path(middle, store, widen, SHIPPED_STORE_PATH, false)
}

fn emitted_len_on_store_path(
    middle: &[u8],
    store: bool,
    widen: bool,
    one_lookup_store: bool,
    seed_history: bool,
) -> usize {
    let arms = force_arms(store, widen, one_lookup_store);
    let mut cpu = flat_cpu(&arms);
    let mut memory = vec![0u8; 0x5000];

    let mut code = vec![0x89, 0xf6];
    code.extend_from_slice(middle);
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    let third = ENTRY + SUBJECT_OFFSET + middle.len() as u32;
    decode_at(&mut cpu, &mut bus, &[ENTRY, ENTRY + SUBJECT_OFFSET, third]);
    // Seeded for every caller but the "the OFF arm is main" one: the heat gate must never be the
    // reason the two arms agree, or the arm comparison would pass with the arms fused.
    if seed_history {
        seed_patch_history(&mut cpu, ENTRY + SUBJECT_OFFSET + 2);
    }
    match jit::direct::compile(&mut cpu, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(c) => c.code.len(),
        jit::direct::CompileOutcome::Retry(cause) => {
            panic!("fixture block did not compile: Retry {cause:?}")
        }
        jit::direct::CompileOutcome::StructuralReject(r) => {
            panic!("fixture block did not compile: reject {r:?}")
        }
    }
}

/// THE OFF ARM IS MAIN, stated as emitted code rather than as an argument — and since the
/// 2026-08-23 flip that arm is the ESCAPE rather than the default, which is exactly when this
/// fixture starts earning its keep.
///
/// Three claims, and the first is the one the flip made load-bearing:
///
/// 1. **the OFF arm emits what a build with no store matcher at all would emit.** `main` at
///    `f6620e6e` had no `disp_store_lane_for`, so it baked every `0x89`/`0x88` displacement
///    whatever its patch history. This fixture reproduces that build's emission twice over, from
///    two independent directions: with the arm OFF and a heat record present, and with the arm ON
///    and no heat record. Both must equal each other, which they can only do by baking. A regression
///    that made the escape emit lane code — or that made the heat gate admit an unpatched slot —
///    breaks the equality;
/// 2. the admitted shapes' two arms must DIFFER, or the escape is decorative;
/// 3. every OTHER shape must emit the same code under both arms, which is what says the arm is a
///    lane admission and not a second code path a non-store block also walks through.
///
/// The refusal list is the admission's own boundary, one entry per bar:
///
/// * the REGISTER forms of the same opcodes (`classify` lowers them to `MovReg`/`MovRegByte`, so
///   they never reach a `Store` or `Load` arm at all);
/// * `0x8A`, which has its own lane, its own knob and its own counter;
/// * a `0x66`-prefixed store, the prefix bar that pins the width argument;
/// * a disp8 store, the `disp_len == 4` bar;
/// * an `ADD [disp32], r32`, an `AluMemDest` — a memory-WRITING kind the store arm deliberately
///   does not admit, and the one refusal that says the arm is keyed on KIND and not on "does it
///   write memory".
#[test]
fn the_off_arm_emits_the_same_code_for_everything_it_does_not_admit() {
    let mut store = STORE_DWORD.to_vec();
    store.extend_from_slice(&TARGETS[0].to_le_bytes());
    let mut byte_store = STORE_BYTE.to_vec();
    byte_store.extend_from_slice(&TARGETS[0].to_le_bytes());
    // CLAIM 1: the escape reproduces main's emission. `emitted_len_under_arms` seeds a heat
    // record, `emitted_len_unseeded` does not; main baked both, so all three must be equal.
    for admitted in [&store, &byte_store] {
        let escape_with_history = emitted_len_under_arms(admitted, false, false);
        assert_eq!(
            escape_with_history,
            emitted_len_unseeded(admitted, true, true),
            "{admitted:02x?}: the escape must emit what a never-patched slot emits under the ON \
             arm, which is what main emitted for every one of them"
        );
        assert_eq!(
            escape_with_history,
            emitted_len_unseeded(admitted, false, false),
            "{admitted:02x?}: and the heat record must make no difference on the escape"
        );
    }
    for admitted in [&store, &byte_store] {
        assert_ne!(
            emitted_len_under_arms(admitted, false, false),
            emitted_len_under_arms(admitted, true, false),
            "{admitted:02x?} must lower differently under the two arms, or the knob does nothing"
        );
    }
    let mut load = LOAD_DWORD.to_vec();
    load.extend_from_slice(&TARGETS[0].to_le_bytes());
    assert_ne!(
        emitted_len_under_arms(&load, false, false),
        emitted_len_under_arms(&load, false, true),
        "the 0x8B widening must lower differently under its two arms"
    );

    // `0x8A [disp32]`, whose own (default-ON) lane the two arms must not disturb.
    let mut laned_8a = vec![0x8a, 0x1d];
    laned_8a.extend_from_slice(&TARGETS[0].to_le_bytes());
    // `66 89 1d disp32`, an operand-size-prefixed store.
    let mut prefixed = vec![0x66, 0x89, 0x1d];
    prefixed.extend_from_slice(&TARGETS[0].to_le_bytes());
    // `add [disp32], ebx`, an `AluMemDest`.
    let mut alu = vec![0x01, 0x1d];
    alu.extend_from_slice(&TARGETS[0].to_le_bytes());
    for middle in [
        // The register forms of all three admitted opcodes.
        &[0x89, 0xd8][..],
        &[0x88, 0xd8][..],
        &[0x8b, 0xd8][..],
        &laned_8a[..],
        &prefixed[..],
        // `mov [ebp+0x10], ebx`, a disp8 store.
        &[0x89, 0x5d, 0x10][..],
        &alu[..],
    ] {
        assert_eq!(
            emitted_len_under_arms(middle, false, false),
            emitted_len_under_arms(middle, true, true),
            "{middle:02x?} must emit the same code with both Option D arms on and off"
        );
    }
}

/// The shape bars, read off the compiled block rather than off emitted length, so a refusal is
/// attributed to the counter it would have moved.
///
/// EVERY ENTRY IS CHOSEN SO THAT DELETING THE BAR IT TESTS ADMITS A LANE AT A WRONG ADDRESS
/// rather than merely failing some other way — the trap the `0x8A` suite's review found and the
/// reason a three-byte disp8 form is NOT in this list. Each is named on its own line.
#[test]
fn the_store_arm_refuses_every_shape_outside_its_bars() {
    let arms = force_arms(true, true, SHIPPED_STORE_PATH);
    // `mov word [disp32], bx`. SEVEN bytes with the displacement still last, so with the prefix
    // term deleted `len - 4` lands on a real disp byte and the slot takes a lane at a WORD store.
    let mut prefixed = vec![0x66, 0x89, 0x1d];
    prefixed.extend_from_slice(&TARGETS[0].to_le_bytes());
    // `add [disp32], ebx`, an `AluMemDest`: a memory-WRITING kind with a disp32 and no immediate,
    // so ONLY the `DirectKind::Store` destructure refuses it. It is what says the arm is keyed on
    // KIND rather than on "does this instruction write memory".
    let mut alu = vec![0x01, 0x1d];
    alu.extend_from_slice(&TARGETS[0].to_le_bytes());
    // `mov dword [disp32], imm32`. A `Store` with disp32 AND a four-byte immediate, ten bytes, so
    // with `imm_len == 0` deleted `len - 4` lands on the IMMEDIATE: the block would absorb
    // patches of the stored VALUE as displacement writes while running the old value's code.
    // This is also the only non-`0x89`/`0x88` `Store` shape that carries a disp32 at all, so it
    // is the opcode bar's fixture as well.
    let mut store_imm = vec![0xc7, 0x05];
    store_imm.extend_from_slice(&TARGETS[0].to_le_bytes());
    store_imm.extend_from_slice(&0x1122_3344u32.to_le_bytes());
    // `mov [moffs32], eax`. A `Store` whose absolute address is the IMMEDIATE field and whose
    // `disp_len` is zero: with the `disp_len == 4` term deleted this five-byte form would lane
    // its last four bytes, which are the moffs — an address the emitted code does not read
    // through the disp seam at all.
    let mut moffs = vec![0xa3];
    moffs.extend_from_slice(&TARGETS[0].to_le_bytes());
    for middle in [
        &prefixed[..],
        // `mov [esp+0x10], ebx` — disp8, and a FOUR-byte instruction, so with `disp_len == 4`
        // deleted `len - 4` lands on the OPCODE byte and a 4-byte guest write rewriting the
        // instruction itself would be absorbed as a lane accept while the block ran the old code.
        // A three-byte disp8 form would refuse for the wrong reason (`checked_sub(4)` underflows).
        &[0x89, 0x5c, 0x24, 0x10][..],
        &alu[..],
        &store_imm[..],
        &moffs[..],
        // The register forms.
        &[0x89, 0xd8][..],
        &[0x8b, 0xd8][..],
    ] {
        let mut cpu = flat_cpu(&arms);
        let mut memory = vec![0u8; 0x5000];
        let mut code = vec![0x89, 0xf6];
        code.extend_from_slice(middle);
        code.extend_from_slice(&[0x89, 0xff, 0xf4]);
        memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
        let mut bus = test_bus(memory);
        map_flat_pages(&mut cpu, &mut bus);
        let third = ENTRY + SUBJECT_OFFSET + middle.len() as u32;
        decode_at(&mut cpu, &mut bus, &[ENTRY, ENTRY + SUBJECT_OFFSET, third]);
        // Seeded BOTH where a correct lane would sit and where the len-4 arithmetic would put one
        // if a bar were deleted, so no refusal below can be the heat gate's doing.
        for offset in 0..=6 {
            seed_patch_history(&mut cpu, ENTRY + SUBJECT_OFFSET + offset);
        }
        let compilation =
            jit::direct::compile(&mut cpu, ENTRY, true).expect("the fixture block compiles");
        assert_eq!(
            compilation.disp_store_lane_count(),
            0,
            "{middle:02x?} took a store lane"
        );
        assert_eq!(
            compilation.disp_load_widen_lane_count(),
            0,
            "{middle:02x?} took a load-widening lane"
        );
    }
}

/// The `0x8A` family must be untouched by either arm: same lane, same counter, on all four
/// combinations. This is the regression that a widened opcode set inside `disp_lane_for` — the
/// implementation the design's "opcode-set widening" phrase invites — would produce.
#[test]
fn the_shipped_8a_lane_is_unchanged_by_both_arms() {
    for (store, widen) in [(false, false), (true, false), (false, true), (true, true)] {
        let arms = force_arms(store, widen, SHIPPED_STORE_PATH);
        let mut cpu = flat_cpu(&arms);
        let mut bus = test_bus(image([0x8a, 0x1d], TARGETS[0]));
        map_flat_pages(&mut cpu, &mut bus);
        decode_at(&mut cpu, &mut bus, &block_starts());
        seed_patch_history(&mut cpu, LANE);
        let compilation =
            jit::direct::compile(&mut cpu, ENTRY, true).expect("the 0x8A block compiles");
        assert_eq!(
            compilation.disp_lane_count(),
            1,
            "0x8A must still take a DISP lane at ({store}, {widen})"
        );
        assert_eq!(compilation.disp_store_lane_count(), 0);
        assert_eq!(compilation.disp_load_widen_lane_count(), 0);
    }
}

/// ANTI-VACUITY FOR THE STORE-PATH ARM. The two legs of every `state_identity` fixture are only
/// worth running if they compile DIFFERENT code, and that is asserted rather than assumed: with
/// `one_lookup_store` on, `emit_store_fast` emits the bias-probe shape; off, `emit_store`
/// classifies the page kind itself. Both reach the lane through `emit_segmented_linear_address`.
#[test]
fn the_two_store_paths_emit_different_code_for_a_laned_store() {
    let mut store = STORE_DWORD.to_vec();
    store.extend_from_slice(&TARGETS[0].to_le_bytes());
    assert_ne!(
        emitted_len_on_store_path(&store, true, false, true, true),
        emitted_len_on_store_path(&store, true, false, false, true),
        "the two store paths must be different code, or the second leg of every state-identity          fixture is a re-run of the first"
    );
}

/// The DS base for the finite-limit fixture. Non-zero deliberately: `emit_segmented_linear_address`
/// emits the base add ONLY when `descriptor.base != 0`, so with a flat DS that instruction does
/// not exist and no other fixture in this file compiles it downstream of a lane.
const LIMITED_DS_BASE: u32 = 0x1000;
/// ...and the limit, likewise emitted ONLY when `limit != u32::MAX`. `max_start` for a dword store
/// is `limit - 3 = 0x2ffc`.
const LIMITED_DS_LIMIT: u32 = 0x2fff;
/// Displacements for the finite-limit fixture, as SEGMENT OFFSETS. The first three are inside the
/// limit (the third is exactly `max_start`, the boundary the compare is written against); the
/// last is past it, and its linear address `0x4000` is a mapped, writable page, so a missing
/// limit check lands a real store there instead of faulting.
const LIMITED_OFFSETS: [u32; 4] = [0x1000, 0x1ffc, 0x2ffc, 0x3000];

fn limited_ds_cpu(arms: &ArmOverride) -> CpuGsw {
    let mut cpu = flat_cpu(arms);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.base = LIMITED_DS_BASE;
    ds.limit = LIMITED_DS_LIMIT;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu
}

/// THE LANE UNDER A FINITE SEGMENT, which is the one emission shape no other fixture here
/// compiles: `emit_segmented_linear_address` emits its limit compare only when
/// `limit != u32::MAX` and its base add only when `base != 0`, and both sit DOWNSTREAM of
/// `emit_effective_address` — so with a lane they run on the PATCHED displacement.
///
/// Two claims, and the second is the one that would be a miscompile:
///
/// 1. inside the limit, a laned store lands at `base + patched_disp` and the whole architectural
///    state matches an interpreter running the same bytes with no block;
/// 2. patched PAST the limit, the native block takes the `SegmentLimit` side exit — one exit,
///    that guard and no other — leaves EIP on the store with no effect, and writes nothing at the
///    linear address the store would otherwise have reached.
///
/// Claim 2 is what a lane arm that formed the address after the compare, or that skipped the
/// compare because the compile-time `disp` was in range, would fail. Both store paths are run.
#[test]
fn a_laned_store_under_a_finite_segment_limit_matches_the_interpreter_and_exits() {
    for one_lookup_store in [true, false] {
        let arms = force_arms(true, false, one_lookup_store);
        let mut native = limited_ds_cpu(&arms);
        let mut native_bus = test_bus(image(STORE_DWORD, LIMITED_OFFSETS[0]));
        map_flat_pages(&mut native, &mut native_bus);
        decode_at(&mut native, &mut native_bus, &block_starts());
        seed_patch_history(&mut native, LANE);
        let id = install(&mut native, 3);
        assert_eq!(
            native.direct_stall_snapshot().disp_store_lane_registrations,
            1,
            "the finite-segment store must still take a lane (one_lookup_store={one_lookup_store})"
        );

        let mut interpreter = limited_ds_cpu(&arms);
        let mut interpreter_bus = test_bus(image(STORE_DWORD, LIMITED_OFFSETS[0]));
        decode_at(&mut interpreter, &mut interpreter_bus, &block_starts());

        for (round, &offset) in LIMITED_OFFSETS.iter().enumerate() {
            if round != 0 {
                guest_store(&mut native, &mut native_bus, LANE, offset);
                guest_store(&mut interpreter, &mut interpreter_bus, LANE, offset);
            }
            let ebx = 0x7071_7200u32.wrapping_add(round as u32 + 1);
            arm_cpu(&mut native, ebx);
            arm_cpu(&mut interpreter, ebx);

            let block = native
                .jit_direct
                .block(id)
                .expect("a lane write must not retire the block");
            let side_exits = native.perf_counters().jit_direct_side_exits;
            let limit_exits = native.direct_stall_snapshot().side_exit_segment_limit;
            assert!(
                native
                    .try_run_direct_block_for_test(&mut native_bus, block)
                    .unwrap(),
                "native block did not run in round {round}"
            );

            let past_limit = offset > LIMITED_DS_LIMIT - 3;
            // The interpreter runs the whole block inside the limit, and stops BEFORE the store
            // on the last round -- the native leg side-exits there, so one cycle is the state the
            // two are compared at. The fault itself is the interpreter's on the re-run and is not
            // this fixture's subject.
            for _ in 0..if past_limit { 1 } else { 3 } {
                interpreter.cycle(&mut interpreter_bus).unwrap();
            }

            assert_eq!(
                native.registers, interpreter.registers,
                "registers differ at offset {offset:#x} (one_lookup_store={one_lookup_store})"
            );
            assert_eq!(
                native_bus.memory, interpreter_bus.memory,
                "guest memory differs at offset {offset:#x}                  (one_lookup_store={one_lookup_store})"
            );
            let exits = native.perf_counters().jit_direct_side_exits - side_exits;
            let limits = native.direct_stall_snapshot().side_exit_segment_limit - limit_exits;
            if past_limit {
                assert_eq!(exits, 1, "exactly one side exit at offset {offset:#x}");
                assert_eq!(
                    limits, 1,
                    "and it must be the SegmentLimit guard, not some other one"
                );
                assert_eq!(
                    native.registers.eip,
                    ENTRY + SUBJECT_OFFSET,
                    "the side exit must leave EIP on the store, before any effect"
                );
                let linear = (LIMITED_DS_BASE + offset) as usize;
                assert_eq!(
                    &native_bus.memory[linear..linear + 4],
                    &[0u8; 4],
                    "a store past the limit must write NOTHING at the address it would reach"
                );
            } else {
                assert_eq!(limits, 0, "no limit exit inside the limit");
                let linear = (LIMITED_DS_BASE + offset) as usize;
                assert_eq!(
                    &native_bus.memory[linear..linear + 4],
                    &ebx.to_le_bytes(),
                    "the store must land at base + PATCHED displacement ({linear:#x})"
                );
            }
        }
    }
}

/// THE SIB disp32 STORE (`89 1c 85 dd dd dd dd`, `MOV [EAX*4 + disp32], EBX`) — SEVEN bytes with
/// the displacement at offset 3, the one admitted encoding whose disp is NOT two bytes in.
///
/// It pins the `physical + len - 4` lane-start arithmetic, which every six-byte fixture in this
/// file would pass with a fixed `+2` in its place. EAX is zero on entry (`arm_cpu` clears the
/// file), so the store resolves to the bare displacement and the patched-store assertion is the
/// mod-0 fixture's.
///
/// Indexed forms are IN deliberately, for the reason iteration 2 of the `0x8A` slice established:
/// Build patches the indexed forms too, and cutting them cost that slice its whole win. What
/// keeps a never-patched indexed store untaxed is the heat gate, not a shape cut.
#[test]
fn a_sib_disp32_store_lanes_at_the_right_offset() {
    let arms = force_arms(true, false, SHIPPED_STORE_PATH);
    let mut cpu = flat_cpu(&arms);
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x89, 0x1c, 0x85];
    code.extend_from_slice(&TARGETS[0].to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut bus = test_bus(memory);
    map_flat_pages(&mut cpu, &mut bus);
    decode_at(
        &mut cpu,
        &mut bus,
        &[ENTRY, ENTRY + SUBJECT_OFFSET, ENTRY + 9],
    );
    // The lane sits at instruction start + 3, one past where the six-byte form puts it.
    let sib_lane = ENTRY + SUBJECT_OFFSET + 3;
    seed_patch_history(&mut cpu, sib_lane);
    let id = install(&mut cpu, 3);
    assert_eq!(
        cpu.direct_stall_snapshot().disp_store_lane_registrations,
        1,
        "the SIB store must take a store lane"
    );

    guest_store(&mut cpu, &mut bus, sib_lane, TARGETS[1]);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 1);

    let block = cpu
        .jit_direct
        .block(id)
        .expect("the lane write must not retire the SIB block");
    arm_cpu(&mut cpu, 0x6162_6364);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(
        &bus.memory[TARGETS[1] as usize..TARGETS[1] as usize + 4],
        &0x6162_6364u32.to_le_bytes(),
        "the native store must land at the patched SIB displacement"
    );
}

/// The `IZARRAVM_DISP_STORE_LANES` spelling table. The knob caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls — hence the parse function is exercised directly.
#[test]
fn disp_store_lanes_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_disp_store_lanes_arm_for_test;
    assert!(
        parse(Err(VarError::NotPresent)),
        "unset must name the ON arm since the 2026-08-23 ladder flipped this default"
    );
    // THE EMPTY STRING IS OFF WHILE UNSET IS ON, and the two must not be confused. The trap
    // flipped with the default: before it an ON leg had to export `1`, now it is the OFF leg that
    // must export `0`, because nulling a variable in PowerShell leaves it PRESENT and EMPTY --
    // which this table spells OFF. `Remove-Item Env:` is the only true unset.
    assert!(
        !parse(Ok(String::new())),
        "the empty string is the OFF arm even though unset is the ON arm"
    );
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the off arm");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the on arm");
    }
}

/// The `IZARRAVM_DISP_LOAD_WIDEN` spelling table, which is the same table one knob over.
#[test]
fn disp_load_widen_spelling_table() {
    use std::env::VarError;
    let parse = jit::direct::parse_disp_load_widen_arm_for_test;
    assert!(!parse(Err(VarError::NotPresent)));
    assert!(!parse(Ok(String::new())));
    for off in ["", "0", "off", "OFF", " off ", "Off"] {
        assert!(!parse(Ok(off.to_string())), "{off:?} must name the off arm");
    }
    for on in ["1", "on", "ON", " On "] {
        assert!(parse(Ok(on.to_string())), "{on:?} must name the on arm");
    }
}

/// A typo must PANIC rather than silently run the default, for both knobs. A mistyped ladder leg
/// that fell through would be read as "the arm I asked for changed nothing", which is the one
/// wrong conclusion an arm ladder exists to avoid.
#[test]
#[should_panic(expected = "IZARRAVM_DISP_STORE_LANES")]
fn a_mistyped_store_arm_panics() {
    let _ = jit::direct::parse_disp_store_lanes_arm_for_test(Ok("yes".to_string()));
}

#[test]
#[should_panic(expected = "IZARRAVM_DISP_LOAD_WIDEN")]
fn a_mistyped_load_widen_arm_panics() {
    let _ = jit::direct::parse_disp_load_widen_arm_for_test(Ok("true".to_string()));
}

/// THE STORE ARM'S DEFAULT PIN, and it is the one assertion that decides what a shipped binary
/// does with the `0x89` / `0x88` displacement store-lane class.
///
/// Catches a flip of `parse_disp_store_lanes_arm`'s `NotPresent` arm. The default is ON since the
/// 2026-08-23 ladder, which priced it at −24.5% min-wall on `duke3d-586-short` and −25.0% on the
/// deciding long row with the corpus inert; a default that moved back without a ladder would
/// change every shipped binary's lane registration silently.
///
/// It reads the AMBIENT knob deliberately — no override — so the assertion agrees with the
/// ENVIRONMENT rather than with a constant, because this suite is run on BOTH arms: a fixture
/// that hard-asserted "on" would make the OFF-arm suite run impossible by construction. With the
/// variable unset it reduces to "the default is ON", which is the claim it exists for.
#[test]
fn the_store_arm_ships_on_by_default() {
    jit::direct::set_disp_store_lanes_for_test(None);
    let ambient = std::env::var("IZARRAVM_DISP_STORE_LANES");
    let expected = jit::direct::parse_disp_store_lanes_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::disp_store_lanes_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DISP_STORE_LANES={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            expected,
            "IZARRAVM_DISP_STORE_LANES must default ON since the 2026-08-23 Option D ladder; see \
             disp_store_lanes_enabled for the rows that priced it"
        );
    }
}

/// THE LOAD-WIDENING ARM'S DEFAULT PIN, which says the opposite and for a measured reason.
///
/// `0x8B` stays OFF at `MAX_BLOCK_IMM_LANES` = 12: on the same ladder it read 2% WORSE than the
/// store arm alone and roughly tripled both cap counters (`imm_lane_cap_refusals` 3,775 ->
/// 11,369, `disp_lane_cap_refusals` 3,372 -> 7,635), which is the pre-registered "cap result, not
/// lane-class result" signature. At sixteen slots it was the best arm on the row. Flipping this
/// default belongs to the lane-cap re-price and to no other change, which is what this pin holds.
#[test]
fn the_load_widen_arm_ships_off_by_default() {
    jit::direct::set_disp_load_widen_for_test(None);
    let ambient = std::env::var("IZARRAVM_DISP_LOAD_WIDEN");
    let expected = jit::direct::parse_disp_load_widen_arm_for_test(ambient.clone());
    assert_eq!(
        jit::direct::disp_load_widen_enabled(),
        expected,
        "the process-wide reading must agree with the spelling table applied to \
         IZARRAVM_DISP_LOAD_WIDEN={ambient:?}"
    );
    if ambient.is_err() {
        assert!(
            !expected,
            "IZARRAVM_DISP_LOAD_WIDEN must default OFF until the lane-cap re-price; at cap 12 it \
             measured 2% worse than the store arm alone"
        );
    }
}
