// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! `SEGWRITE-V86-EDGE`: a V86 segment-write block may publish its static fallthrough successor,
//! guarded at the exit by a three-instruction selector compare against the target's frozen
//! requirement.
//!
//! Design: `dev_docs/2026-09-01-segwrite-continue-design.md`. Companion refutation this must not
//! repeat: `dev_docs/2026-08-31-l9-segment-chain-design.md` §9 (B1 -- the selector-purity lemma is
//! false in plain real mode).
//!
//! Three red proofs (design §7.1 items 1-3), in order:
//!
//! 1. `a_v86_segment_write_edge_binds_and_chains_in_one_entry` -- the WIN. On `main` (before this
//!    slice) `is_segment_write_block()` is true for block A and `jit_direct_entries` rises by 2
//!    per pass (A, then B through the dispatcher); on this branch it rises by 1 (A chains straight
//!    into B).
//! 2. `unreal_mode_refuses_the_edge_while_v86_binds_it` -- THE UNREAL CASE, the fixture that
//!    answers L9 B1. Plain real mode with a stale 4 GB-limit DS (the classic unreal-mode setup)
//!    must NOT get the relaxation: `successors == [None, None]`, and the >64 KB access through the
//!    preserved limit still succeeds. The identical bytes compiled in V86 DO get the relaxation.
//! 3. `a_mismatched_selector_takes_the_guarded_exit_to_the_dispatcher` -- a live selector that
//!    disagrees with the target's frozen requirement exits through `SegmentSelectorMismatch`, not
//!    `StaticUnbound`, and does not spend a linked transfer.
//!
//! Plus three fixtures added in response to adversarial review (PR #809,
//! `dev_docs/2026-09-01-segwrite-edge-review.md`), closing coverage holes the review's own
//! mutation sweep found (N1, N2) and red-proving what dropping `seg_write_merge`'s `| dirty` term
//! changes (N3):
//!
//! 4. `a_two_segment_write_block_still_publishes_no_successor` -- N2. Relaxing
//!    `dirty_segments.count_ones() == 1` to `>= 1` left every existing test green; a multi-write
//!    head (14.3% of 10rogue's, per L9 §9.1) must still get `[None, None]`.
//! 5. `a_chain_widen_folds_through_a_dirty_predecessor_instead_of_cutting_it` -- N1, design §7.1
//!    item 5, cloned from `cpu_jit_retf_v86_test.rs`'s "a requirement GROWS" shape. Reverting the
//!    `widen_chain_requirement` arm to plain `merge_chain` left every existing test green; this
//!    proves the B2 fold survives a widen instead of cutting the edge.
//! 6. `a_write_only_segment_never_gates_reentry_into_its_own_source_block` -- N3. `seg_write_merge`
//!    no longer OR's the written segment into the merged `used` mask (an earlier revision did);
//!    this proves why not: with the bit present, re-entering the SOURCE block with a live value of
//!    the segment it is about to overwrite -- unrelated to anything the guard checks -- would
//!    refuse the entry through the chain entry check and route it down the data-segment decline
//!    treadmill this design exists to convert out of.

use super::*;

const ENTRY: u32 = 0x100;

/// `mov si,si` / `mov di,di`: two filler slots ahead of the segment write, so the write is never
/// the block's entry slot (an entry-position fixture cannot tell a lowering from a side exit that
/// retired nothing) and the three-instruction floor clears on its own.
const FILL: [[u8; 2]; 2] = [[0x89, 0xf6], [0x89, 0xff]];

fn filler() -> Vec<u8> {
    FILL.concat()
}

/// `mov ds, ax` -- `0x8E /3`, register form.
const MOV_DS_AX: [u8; 2] = [0x8e, 0xd8];

/// Block B's body: `mov al,[bx]` (a byte read through DS, the segment A just wrote), followed by
/// two filler slots so B also clears the three-instruction floor. `[bx]`'s default segment is DS
/// with no override, which is what makes B's `SegmentLayout` PIN DS -- `seg_write_merge`'s own
/// admission bar (design §4.2 clause 2) needs the target to claim the written segment or there is
/// nothing for the guard to compare against.
const READ_DS_BX: [u8; 2] = [0x8a, 0x07];
const FILL_TAIL: [[u8; 2]; 2] = [[0x88, 0xc9], [0x88, 0xd2]];

fn block_b_body() -> Vec<u8> {
    let mut body = READ_DS_BX.to_vec();
    body.extend(FILL_TAIL.concat());
    body
}

/// V86: CR0.PE set, EFLAGS.VM set, CPL cached at 3. `load_segment_checked`'s V86 arm rebuilds the
/// whole segment record from the selector (INV-V86-CANON), which is the lemma this whole slice
/// depends on.
fn v86_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x202 | FLAG_VM | (3 << 12);
    cpu.cpl = 3;
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    assert!(cpu.is_v86_mode(), "the V86 fixture must actually be in V86");
    cpu
}

/// Plain real mode: CR0.PE clear, no V86. `load_segment_real_mode` -- not `load_segment_real` --
/// is what a `MOV Sreg` takes here, and it PRESERVES the cached limit. That omission IS unreal
/// mode, and it is L9 B1: a selector-only guard chains onto a stale baked limit unless it is
/// barred from this population entirely.
fn real_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(0x0700);
    cpu.set_eip(ENTRY);
    assert!(!cpu.is_protected_mode());
    assert!(!cpu.is_v86_mode());
    cpu
}

/// Stamp DS with the classic UNREAL-MODE record: a 4 GB limit and a data-segment access byte, the
/// shape a `LDS`/protected-mode load leaves behind when CR0.PE is dropped afterwards without
/// reloading DS. `load_segment_real_mode` never touches `limit`, so this survives the fixture's
/// later `MOV DS,AX` untouched in real mode and must NOT survive it in V86.
fn stamp_unreal_ds(cpu: &mut CpuGsw) {
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.limit = 0xffff_ffff;
    ds.access = 0x93;
    ds.default_size_32 = false;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
}

fn map_direct_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    let permissions = jit::fast_map::PagePermissions::UNPAGED;
    let read = bus
        .direct_page(page, BusAccessKind::DataRead)
        .unwrap()
        .unwrap();
    assert!(cpu.jit_fast_map.populate_read(
        page,
        page,
        read,
        permissions,
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
        permissions,
        cpu.physical_page_watched(page)
    ));
}

/// 256 KiB: enough for the unreal-mode fixture's past-64K access (`UNREAL_OFFSET`) to sit inside
/// mapped, distinctly-valued memory.
const MEMORY_LEN: u32 = 0x4_0000;

fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_LEN as usize];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// Compile and install a block at `linear`, asserting it actually installs. Every page in
/// `memory_fill`'s range is fast-mapped and every instruction boundary the caller names is
/// pre-decoded, matching the pattern `cpu_jit_direct_test.rs::install_fixture_block` and
/// `cpu_jit_retf_v86_test.rs::install_at` both use.
fn install_at(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
    let key = jit::direct::key_for(cpu, linear, false).expect("entry key");
    let compilation = jit::direct::compile(cpu, linear, false)
        .unwrap_or_else(|| panic!("direct compilation failed at {linear:#x}"));
    // `probe` first: it is what moves the key to `Seen`, which `install` requires, and every
    // install fixture in the tree runs it before `install` for that reason.
    let _ = cpu.jit_direct.probe(key);
    let id = cpu.jit_direct.install(&compilation).unwrap_or_else(|| {
        panic!(
            "direct install failed at {linear:#x}, code_len={}",
            compilation.code.len()
        )
    });
    cpu.jit_direct.block(id).expect("live block")
}

/// Stage one CPU: code at `ENTRY` (`filler + mov ds,ax`) immediately followed by `body_b` at its
/// own linear, every decode line warmed and every page fast-mapped. Returns the CPU, its bus, and
/// block B's linear address.
fn stage(builder: fn() -> CpuGsw, body_b: &[u8]) -> (CpuGsw, TestBus, u32) {
    let mut code = filler();
    let mut starts = vec![ENTRY, ENTRY + 2, ENTRY + 4];
    code.extend_from_slice(&MOV_DS_AX);
    let b_linear = ENTRY + code.len() as u32;
    for i in 0..(body_b.len() / 2) {
        starts.push(b_linear + 2 * i as u32);
    }
    code.extend_from_slice(body_b);
    code.push(0xf4); // HLT, so a chain that runs off the end of B halts rather than decoding junk.
    starts.push(b_linear + body_b.len() as u32);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    // A recognisable byte at [BX] under the TARGET selector (0x0040 << 4 == 0x400), read by B's
    // `mov al,[bx]`.
    memory[0x0400] = 0xa5;

    let mut cpu = builder();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_ebx(0);
    for &linear in &starts {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    for page in (0..MEMORY_LEN).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);
    (cpu, bus, b_linear)
}

/// The selector block B's `mov al,[bx]` is compiled to expect.
const TARGET_SELECTOR: u16 = 0x0040;

// =================================================================================================
// Red proof 1: the win -- V86 binds the edge and chains A into B in one entry.
// =================================================================================================

#[test]
fn a_v86_segment_write_edge_binds_and_chains_in_one_entry() {
    let (mut cpu, mut bus, b_linear) = stage(v86_cpu, &block_b_body());

    let block_a = install_at(&mut cpu, ENTRY);
    assert!(
        !block_a.is_segment_write_block(),
        "an eligible V86 segment-write block must publish a successor, not [None, None]"
    );

    // Compile B with DS = TARGET_SELECTOR live, so its `SegmentLayout` freezes exactly that
    // selector as the requirement `chain_layouts[B].selector(Ds)` -- the value the emitted guard
    // will compare A's live post-write selector against.
    cpu.load_segment_real(SegmentIndex::Ds, TARGET_SELECTOR);
    let block_b = install_at(&mut cpu, b_linear);

    // Enter A with AX == the selector B was compiled under: the guard must pass and the two
    // blocks must run as ONE dispatcher entry.
    cpu.registers.set_eax(u32::from(TARGET_SELECTOR));
    cpu.load_segment_real(SegmentIndex::Ds, 0); // whatever DS held before the write is irrelevant
    cpu.set_eip(ENTRY);
    cpu.halted = false;

    let entries_before = cpu.perf_counters().jit_direct_entries;
    let transfers_before = cpu.perf_counters().jit_direct_linked_transfers;
    let mismatches_before = cpu.perf_counters().jit_direct_seg_guard_mismatch_exits;

    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block_a)
            .unwrap()
    );

    assert_eq!(
        cpu.perf_counters().jit_direct_entries - entries_before,
        1,
        "A and B must run in ONE run_direct_block entry, not two"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers - transfers_before,
        1,
        "the chain hop from A into B must be counted as a linked transfer"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_seg_guard_mismatch_exits,
        mismatches_before,
        "a passing guard must not be counted as a mismatch"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        TARGET_SELECTOR,
        "A's write must have taken"
    );
    assert_eq!(
        cpu.registers.eax() & 0xff,
        0xa5,
        "B must have run and read the byte at the NEW DS:[BX]"
    );
    let _ = block_b; // kept for symmetry with the other fixtures; not read again here.
}

// =================================================================================================
// Red proof 2: the unreal case -- L9's B1, answered.
// =================================================================================================

#[test]
fn unreal_mode_refuses_the_edge_while_v86_binds_it() {
    // Real mode with a stale 4 GB-limit DS: `load_segment_real_mode` preserves that limit across
    // the block's own `MOV DS,AX`, which is unreal mode by definition. The relaxation must NOT
    // admit this population -- it is exactly L9 B1.
    let (mut cpu, _bus, _b_linear) = stage(real_cpu, &block_b_body());
    stamp_unreal_ds(&mut cpu);
    let block = install_at(&mut cpu, ENTRY);
    assert!(
        block.is_segment_write_block(),
        "plain real mode must keep the [None, None] bar even with an eligible-shaped block"
    );
    assert_eq!(
        block.successors_for_test(),
        [None, None],
        "the edge must be refused outright in real mode -- this is the B1 population"
    );

    // The access past 64 KB must still succeed, because the limit an ACTUAL `MOV DS,AX` in plain
    // real mode inherits is the stale 4 GB one, not a freshly rebuilt 0xFFFF. Proved through the
    // interpreter directly (this fixture's job is the LINK bar, not re-proving the lowering's own
    // fault surface, which `cpu_jit_seg_load_mem_test.rs` and the design's §5.1 already cover):
    // a fresh real-mode CPU with the same stale DS runs the identical `mov ds,ax` bytes and the
    // limit must survive.
    let mut interp = real_cpu();
    stamp_unreal_ds(&mut interp);
    interp.set_eip(0);
    let mut interp_bus = TestBus::with_memory(vec![0x8e, 0xd8, 0xf4]);
    interp.registers.set_eax(u32::from(TARGET_SELECTOR));
    interp.cycle(&mut interp_bus).unwrap();
    assert_eq!(
        interp.registers.segment(SegmentIndex::Ds).limit,
        0xffff_ffff,
        "a real-mode MOV Sreg,r16 must preserve the cached limit -- the defining property of \
         unreal mode, and the reason B1 killed a selector-only guard here"
    );

    // The identical bytes, compiled in V86: the edge MUST bind. `load_segment_real` (V86's own
    // segment-load path) rebuilds the record from the selector alone, so INV-V86-CANON holds and
    // the guard is sufficient.
    //
    // RED-PROOF MUTATION NOTE: deleting the `key.mode_key & JIT_MODE_KEY_V86_BIT != 0` conjunct
    // from `seg_write_edge_eligible` (`jit/direct.rs`) makes the REAL-MODE assertion above go
    // green for the wrong reason -- `successors_for_test()` would then read
    // `[Some(fallthrough), None]` in real mode too, and a subsequent run would either read the
    // WRONG address (a rebuilt 0xFFFF limit truncating the >64K access) or, if the limit happened
    // to survive by accident, would still have removed the one bar this fixture exists to prove.
    let (mut v86, _v86_bus, v86_b_linear) = stage(v86_cpu, &block_b_body());
    let v86_block = install_at(&mut v86, ENTRY);
    assert!(
        !v86_block.is_segment_write_block(),
        "the identical bytes must get the relaxation in V86"
    );
    assert_ne!(
        v86_block.successors_for_test(),
        [None, None],
        "V86 must publish the static fallthrough this real-mode block was refused"
    );
    let _ = v86_b_linear;
}

// =================================================================================================
// Red proof 3: a live mismatch takes the guarded exit, not StaticUnbound, and spends no transfer.
// =================================================================================================

#[test]
fn a_mismatched_selector_takes_the_guarded_exit_to_the_dispatcher() {
    let (mut cpu, mut bus, b_linear) = stage(v86_cpu, &block_b_body());
    let block_a = install_at(&mut cpu, ENTRY);

    cpu.load_segment_real(SegmentIndex::Ds, TARGET_SELECTOR);
    install_at(&mut cpu, b_linear);

    // A DIFFERENT selector than B's frozen requirement.
    const WRONG_SELECTOR: u16 = 0x0050;
    assert_ne!(WRONG_SELECTOR, TARGET_SELECTOR);
    cpu.registers.set_eax(u32::from(WRONG_SELECTOR));
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.set_eip(ENTRY);
    cpu.halted = false;

    let entries_before = cpu.perf_counters().jit_direct_entries;
    let transfers_before = cpu.perf_counters().jit_direct_linked_transfers;
    let mismatches_before = cpu.perf_counters().jit_direct_seg_guard_mismatch_exits;
    let static_unbound_before = cpu.perf_counters().jit_direct_unresolved_static_unbound;

    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block_a)
            .unwrap()
    );

    assert_eq!(
        cpu.perf_counters().jit_direct_entries - entries_before,
        1,
        "the mismatch is a completed-path exit through the dispatcher, still one entry for A"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_linked_transfers,
        transfers_before,
        "a mismatch must NOT be counted as a linked transfer"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_seg_guard_mismatch_exits - mismatches_before,
        1,
        "the mismatch must be classified under its own reason"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_unresolved_static_unbound,
        static_unbound_before,
        "the mismatch must NOT be folded into StaticUnbound -- that is exactly what a distinct \
         UnresolvedReason variant exists to prevent"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        WRONG_SELECTOR,
        "A's write must still have taken -- the guard runs strictly AFTER every guest-visible \
         effect of the block has committed"
    );
    assert_eq!(
        cpu.registers.eip, b_linear,
        "EIP must sit at the block's normal end (B's start), exactly as an ordinary unbound exit \
         would leave it"
    );
}

// =================================================================================================
// N2: the multi-write bar. Relaxing dirty_segments.count_ones() == 1 to >= 1 is invisible to the
// rest of the suite; this is the fixture that catches it.
// =================================================================================================

/// `mov es, bx` -- `0x8E /0`, register form. Writes a SECOND segment after `MOV_DS_AX` without
/// tripping the compile walk's dirty-segment rule: `LoadSegReal` bakes nothing (`written_segment`
/// is deliberately excluded from `pinned_segments`), so this instruction's own `pinned_segments()`
/// does not intersect `dirty_segments` after the DS write, and the walk keeps going.
const MOV_ES_BX: [u8; 2] = [0x8e, 0xc3];

#[test]
fn a_two_segment_write_block_still_publishes_no_successor() {
    // filler(2) + mov ds,ax + mov es,bx + filler(2): six slots, two segment writes, ending on the
    // ordinary catch-all boundary (decode simply runs out after the tail filler) -- every OTHER
    // `seg_write_edge_eligible` term holds (V86, no CS/SS, no call-out write, plain-fallthrough
    // terminal) except `dirty_segments.count_ones() == 1`.
    let mut code = filler();
    code.extend_from_slice(&MOV_DS_AX);
    code.extend_from_slice(&MOV_ES_BX);
    code.extend(FILL_TAIL.concat());

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut cpu = v86_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);

    let starts: Vec<u32> = (0..(code.len() / 2) as u32)
        .map(|i| ENTRY + 2 * i)
        .collect();
    for &linear in &starts {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("fixture decode");
    }
    for page in (0..MEMORY_LEN).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }
    cpu.set_eip(ENTRY);

    let block = install_at(&mut cpu, ENTRY);
    assert_eq!(
        block.span().instructions,
        6,
        "the walk must actually carry both writes into one block, or this fixture proves nothing"
    );
    assert!(
        block.is_segment_write_block(),
        "a two-segment write must keep the [None, None] bar even though every OTHER eligibility \
         term (V86, plain-fallthrough terminal, no CS/SS, no call-out write) holds"
    );
    assert_eq!(
        block.successors_for_test(),
        [None, None],
        "count_ones() == 1 is a hard bar, not a preference: a source can never owe two guards at \
         one exit"
    );
}

// =================================================================================================
// N1 (design §7.1 item 5): the B2 widen fixture. Reverting `widen_chain_requirement`'s dirty arm
// to plain `merge_chain` is invisible to the rest of the suite; this proves the fold, not the cut.
// =================================================================================================

/// `mov ax, fs` -- `0x8C /4`, register form. Pins FS's SELECTOR (`selector_segment`, part of
/// `pinned_segments`) without addressing memory through it, so it is exactly two bytes like every
/// other slot here and needs no segment-override prefix.
const MOV_AX_FS: [u8; 2] = [0x8c, 0xe0];

#[test]
fn a_chain_widen_folds_through_a_dirty_predecessor_instead_of_cutting_it() {
    // A (writes DS) -> B (reads DS) -> C (pins FS), each compiled and installed in its own pass so
    // the walk cannot fuse two of them into one block. Installing C widens B's OWN chain
    // requirement to include FS -- a segment B's compile-time snapshot never claimed -- and that
    // widen must propagate backward across the A -> B edge through `seg_write_merge`, not the
    // ordinary `merge_chain` L9's own design pass reached for and got wrong (B2).
    let mut code = filler();
    code.extend_from_slice(&MOV_DS_AX);
    let b_linear = ENTRY + code.len() as u32;
    code.extend_from_slice(&block_b_body());
    let c_linear = ENTRY + code.len() as u32;
    code.extend_from_slice(&MOV_AX_FS);
    code.extend(FILL_TAIL.concat());
    code.push(0xf4);

    let mut memory = memory_fill();
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[0x0400] = 0xa5;

    let mut cpu = v86_cpu();
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    cpu.set_fast_map_enabled_for_test(true);
    cpu.registers.set_ebx(0);
    for page in (0..MEMORY_LEN).step_by(0x1000) {
        map_direct_page(&mut cpu, &mut bus, page);
    }

    // A: three slots (two filler, the write).
    for &linear in &[ENTRY, ENTRY + 2, ENTRY + 4] {
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("decode A");
    }
    cpu.set_eip(ENTRY);
    let block_a = install_at(&mut cpu, ENTRY);
    assert!(!block_a.is_segment_write_block());

    // B: three slots, compiled with DS = TARGET_SELECTOR live so it claims exactly that selector
    // -- installing it binds A -> B (unrelated to the widen this fixture is about).
    for i in 0..3u32 {
        let linear = b_linear + 2 * i;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("decode B");
    }
    cpu.load_segment_real(SegmentIndex::Ds, TARGET_SELECTOR);
    cpu.set_eip(b_linear);
    install_at(&mut cpu, b_linear);

    let narrowed_before = cpu.direct_stall_snapshot().chain_requirement_narrowed
        [jit::direct::LinkClearCause::ChainWiden as usize]
        .1;

    // C: three slots. Installing it binds B -> C through the ORDINARY (non-dirty) merge, and
    // because C pins FS -- which B's own compile-time snapshot never touched -- that admission
    // WIDENS B's chain requirement to include FS. `widen_chain_requirement` then walks B's
    // inbound edges, finds A (a dirty, non-far predecessor), and must re-fold A's requirement
    // through `seg_write_merge`, not cut the edge.
    for i in 0..3u32 {
        let linear = c_linear + 2 * i;
        cpu.set_eip(linear);
        cpu.begin_instruction();
        cpu.fetch_decoded(&mut bus, linear).expect("decode C");
    }
    cpu.set_eip(c_linear);
    install_at(&mut cpu, c_linear);

    let narrowed_after = cpu.direct_stall_snapshot().chain_requirement_narrowed
        [jit::direct::LinkClearCause::ChainWiden as usize]
        .1;
    assert_eq!(
        narrowed_after, narrowed_before,
        "the widen must FOLD through the dirty predecessor A via seg_write_merge, not cut the \
         A -> B edge (LinkClearCause::ChainWiden) -- that is the exact shape of L9's own mistake"
    );

    // The A -> B edge must still be LIVE, not merely uncut in the counter: entering A must still
    // chain straight into B in one dispatcher entry.
    cpu.registers.set_eax(u32::from(TARGET_SELECTOR));
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.set_eip(ENTRY);
    cpu.halted = false;
    let entries_before = cpu.perf_counters().jit_direct_entries;
    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block_a)
            .unwrap()
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_entries - entries_before,
        1,
        "the A -> B edge must have survived the widen and still chain in one entry"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        TARGET_SELECTOR,
        "A's write must still have taken"
    );
}

// =================================================================================================
// N3: dropping seg_write_merge's `| dirty` term. Proves what the term changed rather than merely
// asserting the drop is safe.
// =================================================================================================

#[test]
fn a_write_only_segment_never_gates_reentry_into_its_own_source_block() {
    // The same A -> B edge as fixture 1, bound identically.
    let (mut cpu, mut bus, b_linear) = stage(v86_cpu, &block_b_body());
    let block_a = install_at(&mut cpu, ENTRY);
    cpu.load_segment_real(SegmentIndex::Ds, TARGET_SELECTOR);
    install_at(&mut cpu, b_linear);

    // Re-enter A STANDALONE (not chained into, just a plain top-level dispatcher entry) with a
    // live DS that agrees with NEITHER A's original compile-time DS (0, from `stage`) NOR B's
    // frozen requirement (`TARGET_SELECTOR`). A never READS DS -- it only overwrites it -- so
    // this must have no bearing on whether A can be entered at all.
    //
    // If `seg_write_merge` OR'd the written segment into `used` (the reverted shape N3 removes),
    // `chain_layouts[A].used` would claim DS with A's ORIGINAL entry-time descriptor, the armed
    // entry check (`chain_entry_check_armed`, on by default) would compare that against this
    // DIFFERENT live DS, and the entry would be REFUSED
    // (`jit_direct_reject_data_segment`/`_v86`) even though nothing A does depends on DS's value
    // before the write. That refusal is exactly the data-segment decline treadmill this design
    // exists to convert traffic OUT of, aimed at its own population.
    const UNRELATED_DS: u16 = 0x0099;
    assert_ne!(UNRELATED_DS, TARGET_SELECTOR);
    cpu.load_segment_real(SegmentIndex::Ds, UNRELATED_DS);
    cpu.registers.set_eax(u32::from(TARGET_SELECTOR));
    cpu.set_eip(ENTRY);
    cpu.halted = false;

    let entries_before = cpu.perf_counters().jit_direct_entries;
    let reject_before = cpu.perf_counters().jit_direct_reject_data_segment;

    assert!(
        cpu.try_run_direct_block_for_test(&mut bus, block_a)
            .unwrap(),
        "A must still be enterable with an unrelated live DS -- it does not read DS before \
         overwriting it, and the written segment must not be part of A's own entry requirement"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_entries - entries_before,
        1,
        "the entry must actually run natively, not fall back to the interpreter"
    );
    assert_eq!(
        cpu.perf_counters().jit_direct_reject_data_segment,
        reject_before,
        "the written-but-unread segment must not gate A's own entry"
    );
    // And the chain into B still works off the write A performs during this very run, exactly as
    // fixture 1 already covers -- confirmed here as a sanity check that this run actually took
    // the native path rather than silently falling through to the interpreter for some other
    // reason.
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        TARGET_SELECTOR,
        "A's write must have taken, overwriting the unrelated entry-time DS"
    );
}
