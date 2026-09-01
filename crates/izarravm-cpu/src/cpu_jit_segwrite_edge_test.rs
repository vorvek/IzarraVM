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
