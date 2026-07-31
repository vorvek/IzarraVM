// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Mutable imm32 lanes: `ADD r32, imm32` whose immediate is read out of guest RAM on every
//! execution instead of being baked into host code, so a guest patch of those four bytes keeps
//! the compiled block.
//!
//! The fixture is Doom's shape (`README.asm` R_DrawColumn/R_DrawSpan): `81 c5 ii ii ii ii`,
//! `ADD EBP, imm32`, ModRM mod 3 reg 0 rm 5, six bytes, immediate at offset 2.
//!
//! The ADD sits at slot 1, never at the block's entry. An opcode at the entry position is not
//! reached by the emitted body on this fixture path, so an entry-position ADD would leave the
//! lane emitter completely untested while every assertion still passed.

use super::*;

/// Block entry. Chosen so the lane lands 4-byte aligned, which is the alignment Doom's patch
/// store actually has and the one that takes the FastMap write path rather than the fragment
/// fallback.
const ENTRY: u32 = 0x500;
/// Offset of the `ADD EBP, imm32` inside the block: after the two-byte `mov esi, esi`.
const ADD_OFFSET: u32 = 2;
/// The lane: the ADD's immediate field, two bytes into the instruction.
const LANE: u32 = ENTRY + ADD_OFFSET + 2;
/// A second block ENTERS here, in the middle of the first block's ADD, and decodes the lane bytes
/// as instructions of its own. It therefore covers the lane without owning it, which is the case
/// that must still retire.
const ALIAS_ENTRY: u32 = LANE;
/// Four `nop`s, so `ALIAS_ENTRY` decodes as five instructions ending in `mov edi, edi`.
const ALIAS_IMM: u32 = 0x9090_9090;

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.jit_direct.set_fast_map_enabled_for_test(true);
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

/// `mov esi, esi` / `add ebp, imm32` / `mov edi, edi` / `hlt`. The HLT is a hard boundary, so the
/// block is exactly the three instructions before it.
fn image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xff, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
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
    [ENTRY, ENTRY + ADD_OFFSET, ENTRY + ADD_OFFSET + 6]
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
    // Mirrors the production install site in `run.rs`, which is where the registration counter is
    // bumped; a fixture that installed without it would read zero lanes for a lane-bearing block.
    let lanes = compilation.imm_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    id
}

fn arm(cpu: &mut CpuGsw, ebp: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ebp(ebp);
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

/// A guest dword store through the ordinary data-write path — the same path a `mov [addr], reg`
/// takes, so it reaches the SMC choke with the physical address already resolved.
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

/// Compile and install the lane block, and hand back everything a patch-then-run needs.
fn lane_fixture(imm: u32) -> (CpuGsw, TestBus, jit::direct::BlockId) {
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(imm));
    decode_at(&mut cpu, &mut bus, &block_starts());
    let id = install(&mut cpu, ENTRY, 3);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "the fixture ADD did not take a lane; every assertion below would be vacuous"
    );
    (cpu, bus, id)
}

/// The emitter: a lane block's result must equal the interpreter's, including after the guest
/// patches the immediate between executions. The interpreter side runs the same bytes with no
/// block at all, so it re-decodes the patched instruction and is the reference by construction.
#[test]
fn lane_add_matches_the_interpreter_across_patches() {
    let patches = [
        0x0000_0001u32,
        0xffff_ffff,
        0x8000_0000,
        0x7fff_ffff,
        0x0002_0000,
        0,
    ];
    let mut native = flat_cpu();
    let mut native_bus = test_bus(image(patches[0]));
    decode_at(&mut native, &mut native_bus, &block_starts());
    let id = install(&mut native, ENTRY, 3);

    let mut interpreter = flat_cpu();
    let mut interpreter_bus = test_bus(image(patches[0]));
    decode_at(&mut interpreter, &mut interpreter_bus, &block_starts());

    for (round, &imm) in patches.iter().enumerate() {
        if round != 0 {
            guest_store(&mut native, &mut native_bus, LANE, imm);
            guest_store(&mut interpreter, &mut interpreter_bus, LANE, imm);
        }
        let ebp = 0x1234_5678u32.wrapping_mul(round as u32 + 1);
        arm(&mut native, ebp);
        arm(&mut interpreter, ebp);

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
            "registers differ after patch {imm:#010x}"
        );
        assert_eq!(
            native.eflags(),
            interpreter.eflags(),
            "EFLAGS differ after patch {imm:#010x}"
        );
        assert_eq!(
            native.pending_flags, interpreter.pending_flags,
            "lazy flags differ after patch {imm:#010x}"
        );
        assert_eq!(
            native.registers.ebp(),
            ebp.wrapping_add(imm),
            "the native ADD did not use the CURRENT immediate {imm:#010x}"
        );
        assert_eq!(
            native_bus.memory, interpreter_bus.memory,
            "guest memory differs after patch {imm:#010x}"
        );
    }
}

/// The accept case: exactly four bytes at exactly the lane start. The block stays installed and
/// the next entry is still native.
#[test]
fn lane_write_preserves_the_owning_block_and_its_native_entry() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
    let before_kills = cpu.perf_counters().smc_narrow_kills;
    guest_store(&mut cpu, &mut bus, LANE, 0x0000_0020);

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
    arm(&mut cpu, 1);
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
        "the block must still be entered natively"
    );
    assert_eq!(cpu.registers.ebp(), 1 + 0x20);
}

/// A lane write is not code churn. Enough patches to cross the heat threshold several times over
/// must leave the heat map untouched, because that demotion pressure is what the lane exists to
/// remove.
#[test]
fn lane_writes_contribute_no_smc_heat() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
    for round in 0..64u32 {
        // Every value distinct from the last, and none equal to the compiled immediate: G2
        // same-value elision never reaches the choke, so an identical repatch would not count.
        guest_store(&mut cpu, &mut bus, LANE, 0x0001_0000 + round);
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
/// This is also the slice's mutation record. Widening the accept to any width that starts at a
/// lane (dropping the `width == IMM_LANE_WIDTH` term) makes this test fail on both the counter
/// and the block-liveness assertion.
#[test]
fn word_write_at_the_lane_start_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
    guest_store_word(&mut cpu, &mut bus, LANE, 0x1234);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 1);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a partial patch of the immediate must retire the block"
    );
}

/// A write to the instruction's OTHER bytes — its opcode and ModRM — is structural and retires the
/// block. It overlaps no lane byte, so it is not even a lane rejection.
#[test]
fn structural_write_to_the_same_instruction_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
    guest_store(&mut cpu, &mut bus, ENTRY, 0x0000_0000);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 0);
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a write to the opcode bytes must retire the block"
    );
}

/// A four-byte write that overlaps the lane but starts one byte early. Straddling is refused for
/// the same reason a partial patch is: the resulting instruction is not the one that compiled.
#[test]
fn straddling_write_over_the_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
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

/// The page-kind guard, stated positively. A lane is created ONLY from the fetch-page cache — the
/// one direct-page cache that cannot hold a device-aperture pointer, because `Bus::direct_page`
/// hands out a video pointer for `DataRead`/`DataWrite` and for no other access kind. With the
/// fetch entry gone but the data caches warm for the same page, the qualifying ADD must compile
/// with a BAKED immediate: correct as ever, just not parameterized.
///
/// This is the review's fix under test. An earlier revision fell back to `data_write_pages` /
/// `data_read_pages` and would create a lane here; restoring that fallback makes this test fail.
#[test]
fn a_page_the_fetch_cache_cannot_see_gets_no_lane() {
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(5));
    decode_at(&mut cpu, &mut bus, &block_starts());
    // Warm the data-write cache for the code page (well past the block's bytes), then drop the
    // fetch entry — the state every code write already leaves behind, since the choke invalidates
    // the fetch page.
    guest_store(&mut cpu, &mut bus, ENTRY + 0x40, 0x1234_5678);
    cpu.fetch_page.invalidate();

    let key = jit::direct::key_for(&cpu, ENTRY, true).unwrap();
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation =
        jit::direct::compile(&mut cpu, ENTRY, true).expect("the block still compiles");
    assert_eq!(
        compilation.imm_lane_count(),
        0,
        "no fetch-cached page, no lane"
    );
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("the baked-immediate block installs");

    arm(&mut cpu, 100);
    let block = cpu.jit_direct.block(id).unwrap();
    assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
    assert_eq!(
        cpu.registers.ebp(),
        105,
        "the baked immediate still applies"
    );

    // And with no lane, the patch that a lane would have absorbed retires the block instead.
    guest_store(&mut cpu, &mut bus, LANE, 9);
    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(cpu.jit_direct.block(id).is_none());
}

/// Device and HLE writes never take the exemption, even when their range is byte-for-byte a lane.
/// They arrive through the value-less choke with no store path behind them.
#[test]
fn device_write_at_the_lane_retires_the_block() {
    let (mut cpu, mut bus, id) = lane_fixture(1);
    cpu.note_device_memory_write_range(LANE, 4);

    assert_eq!(cpu.perf_counters().smc_lane_accepts, 0);
    assert!(
        cpu.jit_direct.block(id).is_none(),
        "a device write must take the normal invalidation path"
    );
    let _ = &mut bus;
}

/// Two blocks over the same bytes: the first owns the lane, the second entered inside the ADD and
/// decoded the immediate's four bytes as four `nop`s of its own. The patch is a lane write for the
/// owner and a structural write for the other, and each gets its own answer from the same store.
#[test]
fn overlapping_block_without_a_lane_retires_while_the_owner_survives() {
    let mut cpu = flat_cpu();
    let mut bus = test_bus(image(ALIAS_IMM));
    let mut starts = block_starts().to_vec();
    // The alias block's own instruction boundaries: four nops then `mov edi, edi`.
    starts.extend((0..4).map(|i| ALIAS_ENTRY + i));
    starts.push(ALIAS_ENTRY + 4);
    decode_at(&mut cpu, &mut bus, &starts);

    let owner = install(&mut cpu, ENTRY, 3);
    let alias = install(&mut cpu, ALIAS_ENTRY, 5);
    assert_eq!(
        cpu.perf_counters().smc_lane_registrations,
        1,
        "only the owner may register a lane"
    );

    guest_store(&mut cpu, &mut bus, LANE, 0x0000_0007);

    let perf = cpu.perf_counters();
    assert_eq!(perf.smc_lane_accepts, 1, "exactly one block claimed it");
    assert!(
        cpu.jit_direct.block(owner).is_some(),
        "the lane owner must survive"
    );
    assert!(
        cpu.jit_direct.block(alias).is_none(),
        "a block covering the lane bytes without a lane must retire"
    );
}

/// The campaign's step-3 differential matrix. Nested here rather than registered beside the other
/// JIT test files so it inherits this module's fixture (`ENTRY`, `LANE`, `flat_cpu`, `image`,
/// `lane_fixture`) instead of duplicating it.
#[path = "cpu_jit_imm_lane_matrix_test.rs"]
mod matrix;
