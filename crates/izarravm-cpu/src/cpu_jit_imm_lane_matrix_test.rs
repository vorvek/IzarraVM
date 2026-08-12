// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Step 3 of the parameterized-native-blocks campaign: the differential matrix around the Doom
//! patch idiom. The nine tests in the parent module cover the mechanism's basics; this file is the
//! adversarial matrix over it.
//!
//! Every test compares a lane-bearing native execution against a BLOCK-FREE interpreter running
//! the same guest bytes from the same state. The interpreter is the reference by construction: it
//! owns no compiled block, so it re-decodes whatever the patch left in memory, and it is the
//! definition of "what the guest should have seen". Comparisons cover architectural registers
//! (EIP included), resolved EFLAGS, the lazy-flag record behind them, the halt latch, and RAM.
//!
//! The fixtures keep the parent module's entry-position rule: every instruction under test sits at
//! slot 1 or later, never at a block entry, or the emitted lane form would go unexercised while
//! every assertion still passed.

use super::*;

/// Deterministic SplitMix64, so a failing round is reproducible from the seed in the assertion
/// message. Not cryptographic and not meant to be.
struct Prng(u64);

impl Prng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) as u32
    }

    /// Half the draws come from the boundary pool, half are uniform. The pool is what makes the
    /// sign, carry and overflow edges of `ADD` actually get hit on both operands; the uniform half
    /// is what stops the battery from only ever exercising the pool.
    fn value(&mut self) -> u32 {
        const POOL: [u32; 12] = [
            0,
            1,
            0xffff_ffff,
            0x7fff_ffff,
            0x8000_0000,
            0x7fff_fffe,
            0x8000_0001,
            0xffff_fffe,
            0x0000_8000,
            0xffff_8000,
            0x0002_0000,
            0x1234_5678,
        ];
        let draw = self.next_u32();
        if draw & 1 == 0 {
            POOL[(draw >> 1) as usize % POOL.len()]
        } else {
            self.next_u32()
        }
    }
}

fn assert_states_match(
    native: &CpuGsw,
    native_bus: &TestBus,
    interpreter: &CpuGsw,
    interpreter_bus: &TestBus,
    context: &str,
) {
    assert_eq!(
        native.registers, interpreter.registers,
        "{context}: registers"
    );
    assert_eq!(native.eflags(), interpreter.eflags(), "{context}: EFLAGS");
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "{context}: lazy flags"
    );
    assert_eq!(native.halted, interpreter.halted, "{context}: halted");
    assert_eq!(
        native_bus.memory, interpreter_bus.memory,
        "{context}: guest memory"
    );
}

/// The parent module's `decode_at`, generalised over the entry and the instruction boundaries.
fn decode_starts(cpu: &mut CpuGsw, bus: &mut TestBus, entry: u32, starts: &[u32]) {
    for &linear in starts {
        cpu.set_eip(linear);
        cpu.fetch_decoded(bus, linear).unwrap();
    }
    cpu.set_eip(entry);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
    bus.trace = BusTrace::default();
}

/// The parent module's `arm`, generalised over the entry and carrying EDI (the second Doom site's
/// destination) as well as EBP.
fn arm_at(cpu: &mut CpuGsw, entry: u32, ebp: u32, edi: u32) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr.fill(0);
    cpu.registers.set_ebp(ebp);
    cpu.registers.set_edi(edi);
    cpu.registers.set_esp(0xc000);
    cpu.registers.eflags = 0x8d7;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(entry);
    cpu.elapsed_clocks = 0;
    cpu.timing_rem = 0;
    cpu.core_clocks_so_far = 0;
}

fn guest_store_byte(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, value: u32) {
    cpu.write_memory_bus_width(
        bus,
        SegmentIndex::Ds,
        linear,
        BusWidth::Byte,
        value,
        BusAccessKind::DataWrite,
    )
    .expect("fixture patch store");
}

fn store_of_width(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, width: u32, value: u32) {
    match width {
        1 => guest_store_byte(cpu, bus, linear, value),
        2 => guest_store_word(cpu, bus, linear, value),
        4 => guest_store(cpu, bus, linear, value),
        other => panic!("unsupported fixture store width {other}"),
    }
}

/// Seed the linear FastMap for one page. A block containing any memory access refuses to compile
/// without `FastMap` storage (`native_bases` is `None` until the first populate), and the emitted
/// store needs the entry at run time or it side-exits as unavailable before the code-watch guard
/// it is here to exercise.
fn warm_fast_map(cpu: &mut CpuGsw, bus: &mut TestBus, linear: u32, physical: u32) {
    let permissions = jit::fast_map::PagePermissions {
        writable: true,
        user: false,
    };
    let read = bus
        .direct_page(physical, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        linear,
        physical,
        read,
        permissions,
        cpu.physical_page_watched(physical)
    ));
    let write = bus
        .direct_page(physical, BusAccessKind::DataWrite)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_write(
        linear,
        physical,
        write,
        permissions,
        cpu.physical_page_watched(physical)
    ));
}

/// Compile and install at `entry` under code-size `d`, asserting the fixture's shape. The parent
/// module's `install` is the `d = true` case; the 16-bit negative controls need `d = false`.
fn install_with_d(cpu: &mut CpuGsw, entry: u32, instructions: u8, d: bool) -> jit::direct::BlockId {
    let key = jit::direct::key_for(cpu, entry, d).expect("fixture entry must be keyable");
    assert!(matches!(
        cpu.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(cpu, entry, d) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(span) => {
            panic!("fixture block at {entry:#x} was structurally rejected: {span:?}")
        }
        jit::direct::CompileOutcome::Retry => panic!("fixture block at {entry:#x} asked for retry"),
    };
    assert_eq!(
        compilation.span.instructions, instructions,
        "fixture block shape changed"
    );
    let lanes = compilation.imm_lane_count() as u64;
    let id = cpu
        .jit_direct
        .install(&compilation)
        .expect("fixture block installs");
    cpu.perf.smc_lane_registrations += lanes;
    id
}

// ============================================================================================
// Row 1 and row 3: the Doom shape — two `ADD r32, imm32` sites patched in one block.
// ============================================================================================

/// `mov esi, esi` / `add ebp, immA` / `add edi, immB` / `mov esi, esi` / `hlt`. Both patch sites
/// are mid-block, which is R_DrawColumn's `patch1`/`patch2` pair in miniature.
const DOOM_LANE_A: u32 = ENTRY + 4;
const DOOM_LANE_B: u32 = ENTRY + 10;

fn doom_image(a: u32, b: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&a.to_le_bytes());
    code.extend_from_slice(&[0x81, 0xc7]);
    code.extend_from_slice(&b.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

fn doom_starts() -> [u32; 4] {
    [ENTRY, ENTRY + 2, ENTRY + 8, ENTRY + 14]
}

struct DoomPair {
    native: CpuGsw,
    native_bus: TestBus,
    id: jit::direct::BlockId,
    interpreter: CpuGsw,
    interpreter_bus: TestBus,
}

impl DoomPair {
    fn new(a: u32, b: u32) -> Self {
        let mut native = flat_cpu();
        let mut native_bus = test_bus(doom_image(a, b));
        decode_starts(&mut native, &mut native_bus, ENTRY, &doom_starts());
        let id = install_with_d(&mut native, ENTRY, 4, true);
        assert_eq!(
            native.perf_counters().smc_lane_registrations,
            2,
            "both Doom-shaped ADDs must take a lane; every assertion below would be vacuous"
        );

        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(doom_image(a, b));
        decode_starts(
            &mut interpreter,
            &mut interpreter_bus,
            ENTRY,
            &doom_starts(),
        );
        Self {
            native,
            native_bus,
            id,
            interpreter,
            interpreter_bus,
        }
    }

    /// Patch one lane on BOTH legs, so the interpreter sees exactly the store the native leg saw.
    fn patch_both(&mut self, lane: u32, value: u32) {
        guest_store(&mut self.native, &mut self.native_bus, lane, value);
        guest_store(
            &mut self.interpreter,
            &mut self.interpreter_bus,
            lane,
            value,
        );
    }

    fn arm_both(&mut self, ebp: u32, edi: u32) {
        arm_at(&mut self.native, ENTRY, ebp, edi);
        arm_at(&mut self.interpreter, ENTRY, ebp, edi);
    }

    /// One pass over the block: natively on one leg, four interpreted instructions on the other.
    fn run_both(&mut self, context: &str) {
        let block = self
            .native
            .jit_direct
            .block(self.id)
            .unwrap_or_else(|| panic!("{context}: the lane block was retired"));
        assert!(
            self.native
                .try_run_direct_block_for_test(&mut self.native_bus, block)
                .unwrap(),
            "{context}: the block did not run natively"
        );
        for _ in 0..4 {
            self.interpreter.cycle(&mut self.interpreter_bus).unwrap();
        }
        assert_states_match(
            &self.native,
            &self.native_bus,
            &self.interpreter,
            &self.interpreter_bus,
            context,
        );
    }
}

/// Matrix row 1. A seeded stream of patch/execute cycles over the Doom shape: patch both sites,
/// run the loop body, compare against the interpreter, repeat. The values include 0, -1,
/// `0x7fffffff`, `0x80000000` and their neighbours on the immediate AND on the destination, so the
/// sign/carry/overflow boundaries are crossed in both directions.
#[test]
fn randomized_doom_patch_cycles_match_the_interpreter() {
    const SEED: u64 = 0x0731_5eed_0731_5eed;
    const ROUNDS: usize = 400;
    let mut rng = Prng::new(SEED);
    let mut pair = DoomPair::new(1, 2);

    for round in 0..ROUNDS {
        let (a, b, ebp, edi) = (rng.value(), rng.value(), rng.value(), rng.value());
        pair.patch_both(DOOM_LANE_A, a);
        pair.patch_both(DOOM_LANE_B, b);
        pair.arm_both(ebp, edi);
        let context =
            format!("seed {SEED:#x} round {round} a={a:#010x} b={b:#010x} ebp={ebp:#010x}");
        pair.run_both(&context);
        assert_eq!(
            pair.native.registers.ebp(),
            ebp.wrapping_add(a),
            "{context}: site A did not use the CURRENT immediate"
        );
        assert_eq!(
            pair.native.registers.edi(),
            edi.wrapping_add(b),
            "{context}: site B did not use the CURRENT immediate"
        );
    }
    assert!(
        pair.native.perf_counters().smc_lane_accepts > ROUNDS as u64,
        "the rounds must actually reach the lane choke"
    );
}

/// Matrix row 3. The two sites are patched with an execution BETWEEN the writes. That ordering is
/// the one that can expose a lane read from a stale copy of the immediate: the intermediate run
/// must see the new A and the OLD B, exactly as the interpreter does.
#[test]
fn execution_between_the_two_patches_sees_new_a_and_old_b() {
    const OLD_A: u32 = 0x0000_0011;
    const OLD_B: u32 = 0x0000_0022;
    const NEW_A: u32 = 0x8000_0001;
    const NEW_B: u32 = 0x7fff_ffff;
    const EBP: u32 = 0x1000_0000;
    const EDI: u32 = 0x2000_0000;
    let mut pair = DoomPair::new(OLD_A, OLD_B);

    for (stage, lane, value, expect_a, expect_b) in [
        ("after patching A only", DOOM_LANE_A, NEW_A, NEW_A, OLD_B),
        ("after patching B too", DOOM_LANE_B, NEW_B, NEW_A, NEW_B),
    ] {
        pair.patch_both(lane, value);
        pair.arm_both(EBP, EDI);
        pair.run_both(stage);
        assert_eq!(
            pair.native.registers.ebp(),
            EBP.wrapping_add(expect_a),
            "{stage}: site A"
        );
        assert_eq!(
            pair.native.registers.edi(),
            EDI.wrapping_add(expect_b),
            "{stage}: site B"
        );
    }
}

// ============================================================================================
// Row 2: partial and straddling patches.
// ============================================================================================

/// One partial-patch case: the store's start relative to the lane, and its width.
struct PartialCase {
    offset: i32,
    width: u32,
}

/// Byte writes at each of the four immediate offsets; word writes aligned and misaligned across
/// both ends of the lane; dword writes straddling by one, two and three bytes in each direction.
fn partial_cases() -> Vec<PartialCase> {
    let mut cases = Vec::new();
    for offset in 0..4 {
        cases.push(PartialCase { offset, width: 1 });
    }
    for offset in -1..=3 {
        cases.push(PartialCase { offset, width: 2 });
    }
    for offset in [-3, -2, -1, 1, 2, 3] {
        cases.push(PartialCase { offset, width: 4 });
    }
    cases
}

/// Matrix row 2. Every case writes a store whose bytes OUTSIDE the lane are byte-for-byte what is
/// already there, so the program afterwards is always the same three instructions with a new
/// immediate. That is deliberate and load-bearing: it removes "the program changed" as an
/// explanation for any divergence and leaves exactly two things under test — that a partial or
/// straddling patch fails closed (the block retires), and that whatever runs next, interpreted or
/// recompiled, uses the patched bytes.
#[test]
fn partial_and_straddling_patches_fail_closed_and_then_run_the_patched_bytes() {
    // The bytes a full lane patch would install. Chosen so no partial write of them can reproduce
    // the original immediate by accident.
    const PATTERN: [u8; 4] = [0xaa, 0xbb, 0xcc, 0xdd];
    const START_EBP: u32 = 0x0f0f_0f0f;
    const ORIGINAL_IMM: u32 = 1;

    for case in partial_cases() {
        let address = (LANE as i32 + case.offset) as u32;
        let label = format!("width {} at lane{:+}", case.width, case.offset);

        // The store's value: the bytes a full patch would leave, read back at the store's own
        // address and width. Non-lane bytes therefore round-trip unchanged.
        let pristine = image(ORIGINAL_IMM);
        let mut fully_patched = pristine.clone();
        fully_patched[LANE as usize..LANE as usize + 4].copy_from_slice(&PATTERN);
        let mut value = 0u32;
        for i in 0..case.width {
            value |= u32::from(fully_patched[(address + i) as usize]) << (8 * i);
        }
        // What memory looks like afterwards, so the expected immediate is derived from the model
        // rather than assumed from the case table.
        let mut after = pristine.clone();
        for i in 0..case.width {
            after[(address + i) as usize] = (value >> (8 * i)) as u8;
        }
        let expected_imm =
            u32::from_le_bytes(after[LANE as usize..LANE as usize + 4].try_into().unwrap());
        assert_ne!(expected_imm, ORIGINAL_IMM, "{label}: the case is vacuous");
        assert_eq!(
            after[ENTRY as usize..LANE as usize],
            pristine[ENTRY as usize..LANE as usize],
            "{label}: the case must leave the opcode and ModRM bytes alone"
        );

        let (mut native, mut native_bus, id) = lane_fixture(ORIGINAL_IMM);
        let accepts_before = native.perf_counters().smc_lane_accepts;
        store_of_width(&mut native, &mut native_bus, address, case.width, value);

        assert_eq!(
            native.perf_counters().smc_lane_accepts,
            accepts_before,
            "{label}: only four bytes at a lane start may be accepted"
        );
        assert!(
            native.jit_direct.block(id).is_none(),
            "{label}: the block must retire"
        );
        assert_eq!(
            native_bus.memory[ENTRY as usize..ENTRY as usize + 12],
            after[ENTRY as usize..ENTRY as usize + 12],
            "{label}: the store did not land as modelled"
        );

        // The oracle: the post-patch bytes, never compiled.
        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(after);
        decode_starts(
            &mut interpreter,
            &mut interpreter_bus,
            ENTRY,
            &block_starts(),
        );

        // Leg 1 — interpreted, because the retire left nothing to enter.
        arm(&mut native, START_EBP);
        arm(&mut interpreter, START_EBP);
        for _ in 0..3 {
            native.cycle(&mut native_bus).unwrap();
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }
        assert_states_match(
            &native,
            &native_bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: interpreted after the retire"),
        );
        assert_eq!(
            native.registers.ebp(),
            START_EBP.wrapping_add(expected_imm),
            "{label}: the interpreted ADD must use the patched immediate"
        );

        // Leg 2 — recompiled, so the new block's lane (or baked immediate) is the patched one.
        decode_starts(&mut native, &mut native_bus, ENTRY, &block_starts());
        let recompiled = install_with_d(&mut native, ENTRY, 3, true);
        arm(&mut native, START_EBP);
        arm(&mut interpreter, START_EBP);
        let block = native
            .jit_direct
            .block(recompiled)
            .expect("the recompiled block installs");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: the recompiled block must run natively"
        );
        for _ in 0..3 {
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }
        assert_states_match(
            &native,
            &native_bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: recompiled after the retire"),
        );
        assert_eq!(
            native.registers.ebp(),
            START_EBP.wrapping_add(expected_imm),
            "{label}: the recompiled ADD must use the patched immediate"
        );
    }
}

// ============================================================================================
// Row 4: paging aliases and remaps.
// ============================================================================================

const PAGE_DIR: u32 = 0x9000;
const PAGE_TABLE: u32 = 0xa000;
/// A second linear page mapped onto the code's physical page 0. Patching through it is the alias
/// case: lanes are keyed on PHYSICAL, so the write must still resolve to the same lane.
const ALIAS_PAGE: u32 = 0x6000;
/// Where the code's linear page is remapped to, to test that a live block cannot answer for a
/// mapping it did not compile against.
const REMAP_FRAME: u32 = 0x7000;

fn put32(memory: &mut [u8], at: u32, value: u32) {
    memory[at as usize..at as usize + 4].copy_from_slice(&value.to_le_bytes());
}

fn read32(memory: &[u8], at: u32) -> u32 {
    u32::from_le_bytes(memory[at as usize..at as usize + 4].try_into().unwrap())
}

/// Identity-map the first sixteen linear pages, then alias `ALIAS_PAGE` onto physical page 0.
fn paged_memory(image: Vec<u8>) -> Vec<u8> {
    let mut memory = image;
    memory.resize(0x10000, 0);
    put32(&mut memory, PAGE_DIR, PAGE_TABLE | 7);
    for page in 0..16u32 {
        put32(&mut memory, PAGE_TABLE + 4 * page, (page << 12) | 7);
    }
    put32(&mut memory, PAGE_TABLE + 4 * (ALIAS_PAGE >> 12), 7);
    memory
}

fn paged_cpu() -> CpuGsw {
    let mut cpu = flat_cpu();
    cpu.control.cr3 = PAGE_DIR;
    cpu.control.cr0 |= CR0_PG;
    cpu
}

/// Matrix row 4, first half. Two linear mappings onto one physical page: the patch arrives through
/// the alias linear address, and the lane must still accept it, because a lane names physical
/// bytes and not a linear address.
#[test]
fn a_patch_through_a_linear_alias_still_hits_the_lane() {
    const PATCH: u32 = 0x0abc_def0;
    const START_EBP: u32 = 0x0001_0000;

    let mut native = paged_cpu();
    let mut native_bus = test_bus(paged_memory(image(1)));
    decode_starts(&mut native, &mut native_bus, ENTRY, &block_starts());
    let id = install_with_d(&mut native, ENTRY, 3, true);
    assert_eq!(native.perf_counters().smc_lane_registrations, 1);

    let mut interpreter = paged_cpu();
    let mut interpreter_bus = test_bus(paged_memory(image(1)));
    decode_starts(
        &mut interpreter,
        &mut interpreter_bus,
        ENTRY,
        &block_starts(),
    );

    // The alias linear address of the lane. Its translation is physical `LANE`, the same four
    // bytes the block compiled against and the same four bytes its emitted code reads.
    guest_store(&mut native, &mut native_bus, ALIAS_PAGE + LANE, PATCH);
    guest_store(
        &mut interpreter,
        &mut interpreter_bus,
        ALIAS_PAGE + LANE,
        PATCH,
    );
    assert_eq!(
        native.perf_counters().smc_lane_accepts,
        1,
        "the alias write must resolve to the lane"
    );
    assert_eq!(read32(&native_bus.memory, LANE), PATCH);

    let block = native
        .jit_direct
        .block(id)
        .expect("an alias lane write must not retire the block");
    arm_at(&mut native, ENTRY, START_EBP, 0);
    arm_at(&mut interpreter, ENTRY, START_EBP, 0);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_states_match(
        &native,
        &native_bus,
        &interpreter,
        &interpreter_bus,
        "after an alias patch",
    );
    assert_eq!(native.registers.ebp(), START_EBP.wrapping_add(PATCH));
}

/// Matrix row 4, second half. Remap the block's OWN linear page onto a different physical frame
/// carrying different code, then flush. This pins what the code actually does rather than what a
/// design note claims: `BlockKey` carries the physical address, so the remapped entry produces a
/// DIFFERENT key, the probe misses, and the stale block — whose lane still names the old frame's
/// bytes — is simply unreachable from the new mapping. Execution follows the new frame.
#[test]
fn remapping_the_code_page_re_keys_instead_of_reusing_the_stale_lane() {
    const OLD_IMM: u32 = 0x0000_0005;
    const NEW_IMM: u32 = 0x0777_0777;
    const LATER_PATCH: u32 = 0x0bad_0bad;
    const START_EBP: u32 = 0x0001_0000;

    let mut memory = paged_memory(image(OLD_IMM));
    // The frame the linear code page will be remapped to, carrying the same three instructions
    // with a different immediate.
    let other = image(NEW_IMM);
    memory[(REMAP_FRAME + ENTRY) as usize..(REMAP_FRAME + ENTRY) as usize + 12]
        .copy_from_slice(&other[ENTRY as usize..ENTRY as usize + 12]);

    let mut native = paged_cpu();
    let mut native_bus = test_bus(memory.clone());
    decode_starts(&mut native, &mut native_bus, ENTRY, &block_starts());
    let id = install_with_d(&mut native, ENTRY, 3, true);
    assert_eq!(native.perf_counters().smc_lane_registrations, 1);
    let old_key = jit::direct::key_for(&native, ENTRY, true).expect("keyable");

    // Point linear page 0 at REMAP_FRAME, then flush the TLB the way a CR3 reload does.
    guest_store(&mut native, &mut native_bus, PAGE_TABLE, REMAP_FRAME | 7);
    native.flush_tlb_and_code_caches();
    decode_starts(&mut native, &mut native_bus, ENTRY, &block_starts());

    // The property under test is that PHYSICAL KEYING is what makes the remap safe, so the block
    // surviving the flush is half the claim and has to be asserted, not narrated. If a future
    // change made a CR3 reload clear the whole block cache instead, the probe below would still
    // miss and the test would still pass while testing something else entirely.
    assert!(
        native.jit_direct.block(id).is_some(),
        "a TLB flush drops links and translations but keeps compiled blocks; the re-key below is \
         the only thing standing between the stale lane and the new mapping"
    );

    let new_key = jit::direct::key_for(&native, ENTRY, true).expect("keyable");
    assert_ne!(
        old_key.physical, new_key.physical,
        "the remap must change the block key's physical component"
    );
    assert!(
        matches!(
            native.jit_direct.probe(new_key),
            jit::direct::BlockProbe::Interpret
        ),
        "the remapped entry must miss the block cache"
    );

    // A patch of the OLD frame's lane must not reach the new mapping's execution at all.
    guest_store(&mut native, &mut native_bus, ALIAS_PAGE + LANE, LATER_PATCH);
    assert_eq!(read32(&native_bus.memory, LANE), LATER_PATCH);

    // Whatever the block cache still holds, execution at ENTRY now follows the NEW frame.
    let mut interpreter = paged_cpu();
    let mut interpreter_bus = test_bus(native_bus.memory.to_vec());
    decode_starts(
        &mut interpreter,
        &mut interpreter_bus,
        ENTRY,
        &block_starts(),
    );
    arm_at(&mut native, ENTRY, START_EBP, 0);
    arm_at(&mut interpreter, ENTRY, START_EBP, 0);
    for _ in 0..3 {
        native.cycle(&mut native_bus).unwrap();
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_states_match(
        &native,
        &native_bus,
        &interpreter,
        &interpreter_bus,
        "after remapping the code page",
    );
    assert_eq!(
        native.registers.ebp(),
        START_EBP.wrapping_add(NEW_IMM),
        "the remapped page's immediate must be the one that applies"
    );
}

// ============================================================================================
// Row 5: faults.
// ============================================================================================

/// A block that ends exactly on a page boundary and branches onto the next page, so the NEXT
/// instruction's fetch is the first access there. The block has to END on a control transfer: the
/// compile walk needs its successor's decode line, so a block that simply runs off the page edge
/// is refused with `CompileOutcome::Retry` and would never reach the emitter at all.
const BOUNDARY_ENTRY: u32 = 0x0ff4;
const BOUNDARY_LANE: u32 = 0x0ff8;
/// Where the terminating `jmp` lands: two bytes into the unmapped page, so the fault is on a fetch
/// and its CR2 is unambiguous.
const BOUNDARY_TARGET: u32 = 0x1002;

fn boundary_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x10000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xeb, 0x02]);
    memory[BOUNDARY_ENTRY as usize..BOUNDARY_ENTRY as usize + code.len()].copy_from_slice(&code);
    assert_eq!(
        BOUNDARY_ENTRY as usize + code.len(),
        0x1000,
        "the block must end exactly on the page boundary"
    );
    memory
}

/// Matrix row 5, first half. The patched ADD is the last instruction on its page and the next
/// instruction's fetch faults. The lane must apply to the ADD, and the fault must be delivered
/// with exactly the interpreter's CR2 and error code.
#[test]
fn a_lane_at_a_page_boundary_applies_before_the_next_fetch_faults() {
    const PATCH: u32 = 0x0000_4321;
    const START_EBP: u32 = 0x0000_1000;

    let mut memory = paged_memory(boundary_image(1));
    // Linear page 1 is not present, so the fetch at 0x1000 is a #PF.
    put32(&mut memory, PAGE_TABLE + 4, 0);

    let mut native = paged_cpu();
    let mut native_bus = test_bus(memory.clone());
    let starts = [
        BOUNDARY_ENTRY,
        BOUNDARY_ENTRY + 2,
        BOUNDARY_ENTRY + 8,
        BOUNDARY_ENTRY + 10,
    ];
    decode_starts(&mut native, &mut native_bus, BOUNDARY_ENTRY, &starts);
    let id = install_with_d(&mut native, BOUNDARY_ENTRY, 4, true);
    assert_eq!(native.perf_counters().smc_lane_registrations, 1);

    let mut interpreter = paged_cpu();
    let mut interpreter_bus = test_bus(memory);
    decode_starts(
        &mut interpreter,
        &mut interpreter_bus,
        BOUNDARY_ENTRY,
        &starts,
    );

    guest_store(&mut native, &mut native_bus, BOUNDARY_LANE, PATCH);
    guest_store(&mut interpreter, &mut interpreter_bus, BOUNDARY_LANE, PATCH);
    assert_eq!(native.perf_counters().smc_lane_accepts, 1);

    let block = native.jit_direct.block(id).expect("the block survives");
    arm_at(&mut native, BOUNDARY_ENTRY, START_EBP, 0);
    arm_at(&mut interpreter, BOUNDARY_ENTRY, START_EBP, 0);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..4 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_states_match(
        &native,
        &native_bus,
        &interpreter,
        &interpreter_bus,
        "at the page boundary, before the faulting fetch",
    );
    assert_eq!(native.registers.ebp(), START_EBP.wrapping_add(PATCH));
    assert_eq!(native.registers.eip, BOUNDARY_TARGET);

    let native_fault = native.fetch_decoded(&mut native_bus, BOUNDARY_TARGET);
    let interpreter_fault = interpreter.fetch_decoded(&mut interpreter_bus, BOUNDARY_TARGET);
    let page_fault = |fault| match fault {
        Err(InternalFault::Exception {
            vector: 14,
            error_code,
        }) => error_code,
        other => panic!("expected #PF on the next page, got {other:?}"),
    };
    assert_eq!(page_fault(native_fault), page_fault(interpreter_fault));
    assert_eq!(native.control.cr2, interpreter.control.cr2);
    assert_eq!(native.control.cr2, BOUNDARY_TARGET);
}

/// Matrix row 5, second half. The patch store itself faults: the code page is read-only and CR0.WP
/// is set, so a supervisor write to it is a #PF. Nothing may commit — not the bytes, and above all
/// not a lane accept, because an accept on an uncommitted write is exactly the state where the
/// block's emitted code and the interpreter's decode could disagree.
#[test]
fn a_faulting_patch_write_accepts_no_lane_and_changes_nothing() {
    const ORIGINAL_IMM: u32 = 0x0000_0007;
    const START_EBP: u32 = 0x0002_0000;

    let mut memory = paged_memory(image(ORIGINAL_IMM));
    // Present, user, NOT writable.
    put32(&mut memory, PAGE_TABLE, 5);

    let mut native = paged_cpu();
    native.control.cr0 |= CR0_WP;
    let mut native_bus = test_bus(memory.clone());
    decode_starts(&mut native, &mut native_bus, ENTRY, &block_starts());
    let id = install_with_d(&mut native, ENTRY, 3, true);
    assert_eq!(native.perf_counters().smc_lane_registrations, 1);

    let mut interpreter = paged_cpu();
    interpreter.control.cr0 |= CR0_WP;
    let mut interpreter_bus = test_bus(memory);
    decode_starts(
        &mut interpreter,
        &mut interpreter_bus,
        ENTRY,
        &block_starts(),
    );

    let before = native_bus.memory.clone();
    let fault = native.write_memory_bus_width(
        &mut native_bus,
        SegmentIndex::Ds,
        LANE,
        BusWidth::Dword,
        0xdead_beef,
        BusAccessKind::DataWrite,
    );
    assert!(
        matches!(fault, Err(InternalFault::Exception { vector: 14, .. })),
        "the patch write must fault, got {fault:?}"
    );
    let interpreter_fault = interpreter.write_memory_bus_width(
        &mut interpreter_bus,
        SegmentIndex::Ds,
        LANE,
        BusWidth::Dword,
        0xdead_beef,
        BusAccessKind::DataWrite,
    );
    assert!(matches!(
        interpreter_fault,
        Err(InternalFault::Exception { vector: 14, .. })
    ));

    let perf = native.perf_counters();
    assert_eq!(
        perf.smc_lane_accepts, 0,
        "a write that never committed must never take the lane exemption"
    );
    assert_eq!(perf.smc_lane_reject_width, 0);
    assert_eq!(perf.smc_lane_reject_address, 0);
    assert_eq!(
        read32(&native_bus.memory, LANE),
        ORIGINAL_IMM,
        "the immediate must be untouched"
    );
    assert_eq!(
        native_bus.memory[ENTRY as usize..ENTRY as usize + 12],
        before[ENTRY as usize..ENTRY as usize + 12]
    );

    let block = native
        .jit_direct
        .block(id)
        .expect("a faulting write must leave the block exactly as it was");
    arm_at(&mut native, ENTRY, START_EBP, 0);
    arm_at(&mut interpreter, ENTRY, START_EBP, 0);
    assert!(
        native
            .try_run_direct_block_for_test(&mut native_bus, block)
            .unwrap()
    );
    for _ in 0..3 {
        interpreter.cycle(&mut interpreter_bus).unwrap();
    }
    assert_states_match(
        &native,
        &native_bus,
        &interpreter,
        &interpreter_bus,
        "after a faulting patch write",
    );
    assert_eq!(native.registers.ebp(), START_EBP.wrapping_add(ORIGINAL_IMM));
}

// ============================================================================================
// Row 6: the clock cap landing inside the patched loop.
// ============================================================================================

/// `mov esi, esi` / `add ebp, imm32` / `jmp $-10`. The ADD is mid-block, and the block is its own
/// successor, so a run under a cap breaks at whichever instruction the budget runs out on.
fn loop_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0xeb, 0xf6]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// Matrix row 6. The budget boundary is swept across the patched loop. Whatever instruction the
/// break lands on, the state must be exactly what a block-free interpreter reaches after the same
/// number of retired instructions — which is the statement "the break never observes a
/// half-applied patch or a stale immediate", made checkable.
#[test]
fn a_cap_break_never_observes_a_stale_or_half_applied_immediate() {
    const PATCH: u32 = 0x0001_0003;
    const START_EBP: u32 = 0x0000_0100;
    // The warm loop settles with exactly two live lane-bearing blocks over the same bytes: the
    // loop is entered at two different points, and each entry compiles its own block, each of
    // which registers the same immediate as its lane. One patch is therefore accepted twice, once
    // per owning block, which is what the accept counter counts. Pinned exactly rather than as
    // "more than zero", so a change in either direction is visible.
    const LIVE_LANE_OWNERS: u64 = 2;

    for cap in [
        7u64, 11, 13, 17, 19, 23, 29, 31, 37, 41, 53, 67, 83, 101, 149, 211, 307, 401,
    ] {
        let mut native = flat_cpu();
        let mut native_bus = test_bus(loop_image(1));
        native.set_jit_auto_admit(true);
        // Warm the loop until it is compiled and its lane is registered. A single long run is not
        // enough: the first run after an `arm` breaks on the eip discontinuity having retired one
        // instruction, so the admission's first-observation step needs several runs to happen.
        arm(&mut native, START_EBP);
        for _ in 0..64 {
            native.run_straight_line(&mut native_bus, 200).unwrap();
        }
        assert_eq!(
            native.perf_counters().smc_lane_registrations,
            LIVE_LANE_OWNERS,
            "cap {cap}: the loop must settle with its lanes registered; the case would be vacuous"
        );

        let accepts_before = native.perf_counters().smc_lane_accepts;
        guest_store(&mut native, &mut native_bus, LANE, PATCH);
        assert_eq!(
            native.perf_counters().smc_lane_accepts - accepts_before,
            LIVE_LANE_OWNERS,
            "cap {cap}: the patch must reach the lane choke and be accepted by every owner"
        );

        // Settle with a full run rather than single `cycle` steps: a run that starts right after
        // a `cycle` breaks having retired one instruction, and a one-instruction window would put
        // every cap in the sweep at the same place.
        for _ in 0..2 {
            native.run_straight_line(&mut native_bus, 200).unwrap();
        }
        // Where in the three-instruction body the measured run begins, so the number of ADDs it
        // retires is known independently of the oracle.
        let phase = [ENTRY, ENTRY + 2, ENTRY + 8]
            .iter()
            .position(|&lin| lin == native.registers.eip)
            .unwrap_or_else(|| panic!("cap {cap}: settled off the loop body"));
        let start = native.registers.clone();
        let start_pending = native.pending_flags;
        // Deltas rather than `reset_perf_counters`: the reset restarts the instruction clock the
        // SMC heat epoch is derived from, which perturbs admission for the very run under test.
        let retired_before = native.perf_counters().instructions;
        let native_before = native.perf_counters().jit_direct_insns;
        native.run_straight_line(&mut native_bus, cap).unwrap();
        let perf = native.perf_counters();
        let retired = perf.instructions - retired_before;
        assert!(retired > 0, "cap {cap}: the run retired nothing");
        // Per cap, not ORed across the sweep: a single cap that happened to enter native code
        // would otherwise carry the whole sweep's non-vacuity claim. The exact native count is
        // left unpinned because it is a function of the timing model, but every cap in the sweep
        // must break inside a run that entered a block.
        assert!(
            perf.jit_direct_insns > native_before,
            "cap {cap}: the capped run never entered native code"
        );

        // The oracle: the same bytes, never compiled, resumed from the same architectural state
        // and stepped the same number of instructions.
        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(loop_image(PATCH));
        arm(&mut interpreter, START_EBP);
        interpreter.registers = start.clone();
        interpreter.pending_flags = start_pending;
        for _ in 0..retired {
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }

        let context = format!("cap {cap} after {retired} instructions");
        assert_states_match(
            &native,
            &native_bus,
            &interpreter,
            &interpreter_bus,
            &context,
        );
        // The loop retires `mov, add, jmp` in that order, so the count of completed ADDs follows
        // from where the run started and how many instructions it retired. The immediate applied
        // must be the patched one every single time.
        let adds = (0..retired as usize)
            .filter(|step| (phase + step) % 3 == 1)
            .count() as u32;
        assert_eq!(
            native.registers.ebp(),
            start.ebp().wrapping_add(PATCH.wrapping_mul(adds)),
            "{context}: the break saw an immediate that was neither the patch nor a whole \
             number of applications of it"
        );
    }
}

// ============================================================================================
// Row 7: a block whose own store patches its own lane.
// ============================================================================================

/// `mov esi, esi` / `mov [ebx], eax` / `mov esi, esi` / `add ebp, imm32` / `mov esi, esi` / `hlt`,
/// with EBX holding the ADD's own immediate field, so the block's store IS the patch. The store is
/// at slot 1 and the ADD at slot 3.
///
/// Two shape constraints are baked in. The store addresses through EBX rather than a bare disp32,
/// because a disp32-only ModRM carries no index and `direct_addr` refuses its zero scale, so that
/// encoding never reaches the emitter at all. And the filler `mov esi, esi` is what puts the lane
/// on a 4-byte boundary: an unaligned dword store side-exits on alignment before it can reach the
/// code-watch guard this fixture exists to exercise (which is also the alignment Doom's real patch
/// store has).
fn store_then_lane_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x89, 0x03, 0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// The same block with the two slots swapped: the ADD runs first, then the store patches the lane
/// it just used. Same alignment and addressing constraints.
fn lane_then_store_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0x89, 0x03, 0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// The UNALIGNED shape, and the one that matters most: `mov esi, esi` / `mov [ebx], eax` /
/// `add ebp, imm32` / `mov esi, esi` / `hlt`, with no filler, so the immediate field starts 2 mod
/// 4.
///
/// Two of Doom's four real patch sites are exactly this — `0x1cae09` and `0x1cb03e` from the step-1
/// trace — and between them they take this path on the order of a million times per run. The
/// emitted store reaches the wide-access page guard BEFORE the code-watch check, and that guard
/// side-exits a misaligned dword store, so the in-flight chain runs through a DIFFERENT side exit
/// than the aligned fixture. The rest of the chain has to hold identically: the interpreter replays
/// the store, the write is accepted as a lane patch, the block survives, and the re-entry reads the
/// new immediate.
fn store_then_unaligned_lane_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x89, 0x03, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// The unaligned shape with the slots swapped. The extra leading `mov esi, esi` is what puts the
/// immediate at 2 mod 4 in this order.
fn unaligned_lane_then_store_image(imm: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x89, 0xf6, 0x81, 0xc5];
    code.extend_from_slice(&imm.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x03, 0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// One in-flight fixture: its bytes, where its lane is, and what the block looks like.
struct InFlightCase {
    label: &'static str,
    build: fn(u32) -> Vec<u8>,
    lane: u32,
    starts: &'static [u32],
    instructions: u8,
    /// The HLT that ends the fixture, used as the stop condition for stepping.
    hlt: u32,
    /// The immediate the ADD uses on the FIRST pass: the new one when the store runs before it,
    /// the original when it runs after.
    first_pass_imm_is_new: bool,
}

/// Step the CPU until it reaches `target_eip`, entering native code where it can. Bounded so a
/// fixture that stops making progress fails instead of hanging.
fn step_until(cpu: &mut CpuGsw, bus: &mut TestBus, target_eip: u32, context: &str) {
    for _ in 0..16 {
        if cpu.registers.eip == target_eip {
            return;
        }
        cpu.cycle(bus).unwrap();
    }
    panic!(
        "{context}: never reached {target_eip:#x} (stuck at {:#x})",
        cpu.registers.eip
    );
}

/// Matrix row 7. The in-flight case the step-2 review reasoned about rather than measured: a
/// block that writes its own lane while it is executing. The reasoning is that the emitted store
/// side-exits BEFORE committing, the interpreter replays the store, and the write therefore
/// reaches the choke with no block mid-execution. This makes each link observable — the side exit
/// fires, the lane still holds the old bytes at the exit, the replayed store is accepted, the
/// block survives, and the re-entered block reads the new immediate.
///
/// Four fixtures: both slot orders, at both alignments. The alignment split is not cosmetic — two
/// of Doom's four real patch sites are misaligned, so the aligned fixture alone would leave the
/// path the guest actually takes about a million times a run unexercised.
///
/// **All four now exit on the CODE WATCH, and the two misaligned rows changed meaning with guard 3
/// rather than merely changing counters.** They used to exit on the wide-access page guard, which
/// refused every misaligned store before the code-watch check could run — so the rows named for
/// the Doom shape were in fact certifying the alignment refusal and never reaching the guard they
/// were built to exercise. The lean store site now serves a page-local misaligned store through
/// its slow stub, which runs `emit_watched_store_guard`, so the self-store really does meet the
/// code watch at both alignments.
///
/// That is why the expected-exit field is gone: there is one answer for all four rows, and each
/// still pins that the OTHER guard did not fire.
#[test]
fn a_block_that_patches_its_own_lane_side_exits_and_survives() {
    const ORIGINAL_IMM: u32 = 0x0000_0003;
    const NEW_IMM: u32 = 0x0055_0011;
    const START_EBP: u32 = 0x0010_0000;

    let cases = [
        InFlightCase {
            label: "aligned, store before the lane",
            build: store_then_lane_image,
            lane: ENTRY + 8,
            starts: &[ENTRY, ENTRY + 2, ENTRY + 4, ENTRY + 6, ENTRY + 12],
            instructions: 5,
            hlt: ENTRY + 14,
            first_pass_imm_is_new: true,
        },
        InFlightCase {
            label: "aligned, lane before the store",
            build: lane_then_store_image,
            lane: ENTRY + 4,
            starts: &[ENTRY, ENTRY + 2, ENTRY + 8, ENTRY + 10, ENTRY + 12],
            instructions: 5,
            hlt: ENTRY + 14,
            first_pass_imm_is_new: false,
        },
        InFlightCase {
            label: "unaligned (the Doom shape), store before the lane",
            build: store_then_unaligned_lane_image,
            lane: ENTRY + 6,
            starts: &[ENTRY, ENTRY + 2, ENTRY + 4, ENTRY + 10],
            instructions: 4,
            hlt: ENTRY + 12,
            first_pass_imm_is_new: true,
        },
        InFlightCase {
            label: "unaligned, lane before the store",
            build: unaligned_lane_then_store_image,
            lane: ENTRY + 6,
            starts: &[ENTRY, ENTRY + 2, ENTRY + 4, ENTRY + 10, ENTRY + 12],
            instructions: 5,
            hlt: ENTRY + 14,
            first_pass_imm_is_new: false,
        },
    ];

    for case in cases {
        let label = case.label;
        let first_pass_imm = if case.first_pass_imm_is_new {
            NEW_IMM
        } else {
            ORIGINAL_IMM
        };

        let mut native = flat_cpu();
        let mut native_bus = test_bus((case.build)(ORIGINAL_IMM));
        decode_starts(&mut native, &mut native_bus, ENTRY, case.starts);
        warm_fast_map(&mut native, &mut native_bus, 0, 0);
        let id = install_with_d(&mut native, ENTRY, case.instructions, true);
        assert_eq!(
            native.perf_counters().smc_lane_registrations,
            1,
            "{label}: the ADD must take a lane"
        );

        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus((case.build)(ORIGINAL_IMM));
        decode_starts(&mut interpreter, &mut interpreter_bus, ENTRY, case.starts);

        // EAX carries the new immediate, so the block's own store is the patch.
        arm_at(&mut native, ENTRY, START_EBP, 0);
        arm_at(&mut interpreter, ENTRY, START_EBP, 0);
        for cpu in [&mut native, &mut interpreter] {
            cpu.registers.set_eax(NEW_IMM);
            cpu.registers.set_ebx(case.lane);
        }

        let before = native.perf_counters().clone();
        let block = native.jit_direct.block(id).expect("installed");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap(),
            "{label}: the block must be entered"
        );
        let after = native.perf_counters().clone();
        let watch = after.jit_direct_exit_code_watch - before.jit_direct_exit_code_watch;
        let alignment = after.jit_direct_exit_cross_page_or_alignment
            - before.jit_direct_exit_cross_page_or_alignment;
        assert!(
            watch > 0,
            "{label}: the store into watched code must side-exit on the code watch"
        );
        assert_eq!(
            alignment, 0,
            "{label}: no self-store may be refused by the page guard first"
        );
        assert_eq!(
            read32(&native_bus.memory, case.lane),
            ORIGINAL_IMM,
            "{label}: the side exit must precede the store's commit"
        );

        // The interpreter replays the store, which is where the write reaches the choke.
        step_until(&mut native, &mut native_bus, case.hlt, label);
        step_until(&mut interpreter, &mut interpreter_bus, case.hlt, label);
        assert_eq!(
            native.perf_counters().smc_lane_accepts,
            before.smc_lane_accepts + 1,
            "{label}: the replayed store must be classified as a lane patch"
        );
        assert!(
            native.jit_direct.block(id).is_some(),
            "{label}: the block must survive its own lane patch"
        );
        assert_states_match(
            &native,
            &native_bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: first pass"),
        );
        assert_eq!(
            native.registers.ebp(),
            START_EBP.wrapping_add(first_pass_imm),
            "{label}: the first pass must use the immediate in force when the ADD ran"
        );
        assert_eq!(read32(&native_bus.memory, case.lane), NEW_IMM);

        // Re-enter: the surviving block must now read the patched immediate.
        arm_at(&mut native, ENTRY, START_EBP, 0);
        arm_at(&mut interpreter, ENTRY, START_EBP, 0);
        for cpu in [&mut native, &mut interpreter] {
            cpu.registers.set_eax(NEW_IMM);
            cpu.registers.set_ebx(case.lane);
        }
        let block = native
            .jit_direct
            .block(id)
            .expect("the block is still installed");
        assert!(
            native
                .try_run_direct_block_for_test(&mut native_bus, block)
                .unwrap()
        );
        step_until(&mut native, &mut native_bus, case.hlt, label);
        step_until(&mut interpreter, &mut interpreter_bus, case.hlt, label);
        assert_states_match(
            &native,
            &native_bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: second pass"),
        );
        assert_eq!(
            native.registers.ebp(),
            START_EBP.wrapping_add(NEW_IMM),
            "{label}: the re-entered block must use the patched immediate"
        );
    }
}

// ============================================================================================
// Row 8: 16-bit negative controls.
// ============================================================================================

/// A 16-bit code segment: the same flat base and limit, `default_size_32` cleared.
fn code_segment_16(cpu: &mut CpuGsw) {
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x08,
            base: 0,
            limit: u32::MAX,
            access: 0x9b,
            default_size_32: false,
        },
    );
}

/// Which arm of the compiler a negative control must land on.
///
/// Pinning the arm is the whole point. Two of the three controls never reach `imm_lane_for` at
/// all, so for them `smc_lane_registrations == 0` is true by construction and would stay true if
/// the classifier were later opened up to these encodings — the assertion would keep passing while
/// the thing it claims to protect had moved. Naming the arm makes the test fail on that change
/// instead of sleeping through it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlArm {
    /// `classify` refuses the encoding outright, so the compile walk abandons the block at that
    /// slot and `imm_lane_for` is never consulted. No block exists to run. No control takes this
    /// arm since the 2026-08-08 `0x81` word admission, but it stays: the matrix's contract is
    /// that each control PINS its arm, and the next admission or revocation re-uses it.
    #[allow(dead_code)]
    RejectedBeforeTheLaneCheck,
    /// The block compiles, installs and is entered natively, and `imm_lane_for` DID see this ADD
    /// and refused it — the word forms on their width, the prefix-carrying dword form on the
    /// prefix and the seven-byte length.
    CompiledWithoutALane,
}

/// Matrix row 8. Three encodings that are NOT `ADD r32, imm32` in the admitted shape:
///
/// - `66 81 /0` in a 32-bit segment: an operand-size override makes it `ADD r16, imm16`.
/// - `81 /0` in a 16-bit segment: the same bytes as the Doom site, but the operand size follows
///   CS.D, so it is a word form with a two-byte immediate.
/// - `66 81 /0` in a 16-bit segment: a genuine 32-bit ADD with a four-byte immediate.
///
/// Since the 2026-08-08 `0x81` word admission all three COMPILE, and the property this row pins
/// is that none of them registers a lane: a lane is Dword-only (`IMM_LANE_WIDTH` is four and
/// `imm_lane_for` matches the width), so the two word forms and the prefix-carrying dword form
/// all take `CompiledWithoutALane`. Each control pins its arm, so a lane-side change that started
/// accepting any of them would fail here rather than pass quietly. All three must also execute
/// correctly, patched or not — a patch of a lane-free immediate retires the block.
#[test]
fn sixteen_bit_add_forms_never_register_a_lane() {
    struct Control {
        label: &'static str,
        d: bool,
        mode: GswMode,
        /// The ADD's bytes, immediate excluded.
        prefix: &'static [u8],
        /// Immediate width in bytes.
        imm_len: u32,
        arm: ControlArm,
    }
    let controls = [
        Control {
            label: "0x66-prefixed word ADD in a 32-bit segment",
            d: true,
            mode: GswMode::Gsw586,
            prefix: &[0x66, 0x81, 0xc5],
            imm_len: 2,
            arm: ControlArm::CompiledWithoutALane,
        },
        Control {
            label: "unprefixed word ADD in a 16-bit segment",
            d: false,
            mode: GswMode::Gsw586,
            prefix: &[0x81, 0xc5],
            imm_len: 2,
            arm: ControlArm::CompiledWithoutALane,
        },
        Control {
            label: "0x66-prefixed dword ADD in a 16-bit segment",
            d: false,
            mode: GswMode::Gsw586,
            prefix: &[0x66, 0x81, 0xc5],
            imm_len: 4,
            arm: ControlArm::CompiledWithoutALane,
        },
    ];

    for control in controls {
        let label = control.label;
        let add_len = control.prefix.len() as u32 + control.imm_len;
        let imm_at = ENTRY + 2 + control.prefix.len() as u32;
        let build = |imm: u32| {
            let mut memory = vec![0u8; 0x5000];
            let mut code = vec![0x89, 0xf6];
            code.extend_from_slice(control.prefix);
            code.extend_from_slice(&imm.to_le_bytes()[..control.imm_len as usize]);
            code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
            memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            memory
        };
        let starts = [ENTRY, ENTRY + 2, ENTRY + 2 + add_len];

        let mut cpu = flat_cpu();
        cpu.set_mode(control.mode);
        if !control.d {
            code_segment_16(&mut cpu);
        }
        let mut bus = test_bus(build(0x0000_1111));
        decode_starts(&mut cpu, &mut bus, ENTRY, &starts);

        // The probe comes first, exactly as the production dispatch does it: the cache records the
        // key on the miss, and `install` refuses a compilation whose key it has never seen.
        let key = jit::direct::key_for(&cpu, ENTRY, control.d)
            .unwrap_or_else(|| panic!("{label}: the entry must be keyable at all"));
        assert!(
            matches!(
                cpu.jit_direct.probe(key),
                jit::direct::BlockProbe::Interpret
            ),
            "{label}: a fresh cache must miss"
        );

        let installed = match (
            control.arm,
            jit::direct::compile(&mut cpu, ENTRY, control.d),
        ) {
            (
                ControlArm::RejectedBeforeTheLaneCheck,
                jit::direct::CompileOutcome::StructuralReject(_),
            ) => None,
            (ControlArm::RejectedBeforeTheLaneCheck, _) => panic!(
                "{label}: expected a structural reject — the ALU-group immediate forms are not in \
                 the OperandSize::Word allowlist, so the walk must abandon the block at this slot"
            ),
            (
                ControlArm::CompiledWithoutALane,
                jit::direct::CompileOutcome::Compiled(compilation),
            ) => {
                assert_eq!(
                    compilation.imm_lane_count(),
                    0,
                    "{label}: a non-admitted encoding registered a lane"
                );
                assert_eq!(
                    compilation.span.instructions, 3,
                    "{label}: fixture block shape changed"
                );
                Some(
                    cpu.jit_direct
                        .install(&compilation)
                        .unwrap_or_else(|| panic!("{label}: the lane-free block must install")),
                )
            }
            (ControlArm::CompiledWithoutALane, _) => panic!(
                "{label}: expected a compiled block whose ADD reached imm_lane_for and was refused"
            ),
        };
        assert_eq!(
            cpu.perf_counters().smc_lane_registrations,
            0,
            "{label}: no lane may be registered"
        );

        // The oracle: the same bytes, never compiled.
        let mut interpreter = flat_cpu();
        interpreter.set_mode(control.mode);
        if !control.d {
            code_segment_16(&mut interpreter);
        }
        let mut interpreter_bus = test_bus(build(0x0000_1111));
        decode_starts(&mut interpreter, &mut interpreter_bus, ENTRY, &starts);

        // Pass 1. Where a block exists it is ENTERED NATIVELY, so the comparison is native against
        // interpreter rather than interpreter against interpreter.
        arm_at(&mut cpu, ENTRY, 0x0000_0100, 0);
        arm_at(&mut interpreter, ENTRY, 0x0000_0100, 0);
        if let Some(id) = installed {
            let native_before = cpu.perf_counters().jit_direct_insns;
            let block = cpu.jit_direct.block(id).expect("installed");
            assert!(
                cpu.try_run_direct_block_for_test(&mut bus, block).unwrap(),
                "{label}: the lane-free block must run natively"
            );
            assert!(
                cpu.perf_counters().jit_direct_insns > native_before,
                "{label}: the native leg retired nothing"
            );
        } else {
            for _ in 0..3 {
                cpu.cycle(&mut bus).unwrap();
            }
        }
        for _ in 0..3 {
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }
        assert_states_match(
            &cpu,
            &bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: unpatched"),
        );

        // Pass 2. Patch the immediate field. With no lane the write is ordinary code churn, so any
        // block must retire and the next entry is interpreted.
        store_of_width(&mut cpu, &mut bus, imm_at, control.imm_len, 0x0000_2222);
        store_of_width(
            &mut interpreter,
            &mut interpreter_bus,
            imm_at,
            control.imm_len,
            0x0000_2222,
        );
        if let Some(id) = installed {
            assert!(
                cpu.jit_direct.block(id).is_none(),
                "{label}: with no lane, a patch of the immediate must retire the block"
            );
        }
        arm_at(&mut cpu, ENTRY, 0x0000_0100, 0);
        arm_at(&mut interpreter, ENTRY, 0x0000_0100, 0);
        for _ in 0..3 {
            cpu.cycle(&mut bus).unwrap();
            interpreter.cycle(&mut interpreter_bus).unwrap();
        }
        assert_states_match(
            &cpu,
            &bus,
            &interpreter,
            &interpreter_bus,
            &format!("{label}: patched"),
        );
        assert_eq!(
            cpu.perf_counters().smc_lane_accepts,
            0,
            "{label}: no write may be accepted as a lane patch"
        );
    }
}

// ============================================================================================
// The SMC trace instrument alongside the lanes.
// ============================================================================================

/// The trace's `covering_line` probe runs BEFORE the lane classification in the same choke, so the
/// two could in principle interfere. This runs one fixed patch/execute script twice, with the
/// trace off and on, and requires the guest result and the whole lane counter trio to be identical
/// — while the trace still records the events, so the comparison is not between two silent runs.
/// What one run of the trace script produced.
struct TraceScriptOutcome {
    /// Registrations, accepts, width rejections, address rejections.
    lane_counters: (u64, u64, u64, u64),
    ebp: u32,
    memory: Vec<u8>,
    report: Option<Vec<String>>,
}

#[test]
fn the_smc_trace_does_not_disturb_lane_classification() {
    fn script(trace: bool) -> TraceScriptOutcome {
        let (mut cpu, mut bus, id) = lane_fixture(1);
        if trace {
            cpu.set_smc_trace_enabled(true);
        }
        let mut ebp = 0x0001_0000u32;
        for round in 0..8u32 {
            let value = 0x0010_0000 + round;
            guest_store(&mut cpu, &mut bus, LANE, value);
            arm(&mut cpu, ebp);
            let block = cpu.jit_direct.block(id).expect("the block survives");
            assert!(cpu.try_run_direct_block_for_test(&mut bus, block).unwrap());
            ebp = cpu.registers.ebp();
        }
        // A word patch at the lane start: the width rejection, so the trace sees a retire too.
        guest_store_word(&mut cpu, &mut bus, LANE, 0x4321);
        assert!(cpu.jit_direct.block(id).is_none());
        let perf = cpu.perf_counters();
        let lane_counters = (
            perf.smc_lane_registrations,
            perf.smc_lane_accepts,
            perf.smc_lane_reject_width,
            perf.smc_lane_reject_address,
        );
        let report = cpu.take_smc_trace_report();
        TraceScriptOutcome {
            lane_counters,
            ebp,
            memory: bus.memory.to_vec(),
            report,
        }
    }

    let off = script(false);
    let on = script(true);
    assert!(off.report.is_none(), "the trace is off by default");
    assert!(
        on.report.is_some_and(|lines| !lines.is_empty()),
        "the traced run must actually record events, or the comparison is between two silent runs"
    );
    assert_eq!(
        off.lane_counters, on.lane_counters,
        "lane counters moved with the trace"
    );
    assert_eq!(off.ebp, on.ebp, "the guest result moved with the trace");
    assert_eq!(off.memory, on.memory, "guest memory moved with the trace");
}

// ============================================================================================
// The widened `0x81 /r` lane family: every ALU-group member, not only /0 ADD.
// ============================================================================================

/// `doom_image` with the ModRM reg field parameterized: `81 /op EBP, immA` / `81 /op EDI, immB`
/// at the same offsets, so `DOOM_LANE_A`/`DOOM_LANE_B` and `doom_starts` apply unchanged.
fn family_image(op: u8, a: u32, b: u32) -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    let mut code = vec![0x89, 0xf6, 0x81, 0xc0 | (op << 3) | 5];
    code.extend_from_slice(&a.to_le_bytes());
    code.extend_from_slice(&[0x81, 0xc0 | (op << 3) | 7]);
    code.extend_from_slice(&b.to_le_bytes());
    code.extend_from_slice(&[0x89, 0xf6, 0xf4]);
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory
}

/// Every `0x81 /r` group member takes a lane and its laned execution matches the block-free
/// interpreter across patch cycles — the widening measured out of duke3d's SMC shape census
/// (31.7M of its 37.2M imm-field patch events are `/3, /5, /2, /0`; the old lane admitted `/0`
/// alone). Both carry polarities run: ADC (/2) and SBB (/3) consume the incoming CF, and a fixed
/// arm state would leave one polarity of `emit_carry_alu_preloaded` unexecuted. CMP (/7) rides
/// along as the non-writing member — the state compare is what validates it.
#[test]
fn widened_alu_family_lanes_match_the_interpreter() {
    const SEED: u64 = 0x0808_2026_d00b_1e5e;
    const ROUNDS: usize = 64;
    for op in 1u8..=7 {
        let mut rng = Prng::new(SEED ^ u64::from(op));
        let mut native = flat_cpu();
        let mut native_bus = test_bus(family_image(op, 1, 2));
        decode_starts(&mut native, &mut native_bus, ENTRY, &doom_starts());
        let id = install_with_d(&mut native, ENTRY, 4, true);
        assert_eq!(
            native.perf_counters().smc_lane_registrations,
            2,
            "op /{op}: both sites must take a lane; every assertion below would be vacuous"
        );
        let mut interpreter = flat_cpu();
        let mut interpreter_bus = test_bus(family_image(op, 1, 2));
        decode_starts(
            &mut interpreter,
            &mut interpreter_bus,
            ENTRY,
            &doom_starts(),
        );
        let mut pair = DoomPair {
            native,
            native_bus,
            id,
            interpreter,
            interpreter_bus,
        };
        for round in 0..ROUNDS {
            let (a, b, ebp, edi) = (rng.value(), rng.value(), rng.value(), rng.value());
            pair.patch_both(DOOM_LANE_A, a);
            pair.patch_both(DOOM_LANE_B, b);
            pair.arm_both(ebp, edi);
            let eflags = if round & 1 == 1 { 0x8d7 } else { 0x8d6 };
            pair.native.registers.eflags = eflags;
            pair.interpreter.registers.eflags = eflags;
            pair.run_both(&format!(
                "op /{op} seed {SEED:#x} round {round} a={a:#010x} b={b:#010x} \
                 ebp={ebp:#010x} edi={edi:#010x} cf={}",
                eflags & 1
            ));
        }
        // A small deficit against 2*ROUNDS is expected, not suspicious: a round whose drawn
        // value equals the value already in the field is skipped by the same-value dedup (G2)
        // BEFORE the lane choke, so it neither accepts nor kills. The bound still proves the
        // overwhelming majority of patches reached the lanes.
        assert!(
            pair.native.perf_counters().smc_lane_accepts >= 2 * (ROUNDS as u64) - 8,
            "op /{op}: the rounds must actually reach the lane choke, accepts={}",
            pair.native.perf_counters().smc_lane_accepts,
        );
    }
}
