// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The CR0 code-translation flush predicate (`cr0_write_moves_code_translation`).
//!
//! A CR0 write only moves the linear->physical map through PG. Every other bit leaves the map
//! identical, so the decode cache and the Direct link graph -- both of which are keyed on a
//! linear address plus a mode key that already carries PE -- survive the write. Tyrian 2000's
//! real<->protected thunk writes CR0 464 times a guest second and paid a whole-cache flush for
//! each one.
//!
//! The rows below are the design's red-first list (dev_docs/specs/2026-08-29-flush-storm-design.md
//! section 7). Three families:
//!
//! 1. the KEEP rows (`cr0_pe_toggle_*`, `lmsw_never_flushes_for_any_operand`,
//!    `pmode_entry_reuses_a_real_mode_decode_line_and_still_faults`) -- red before the slice;
//! 2. the ANTI-REGRESSION rows (`cr0_pg_*`, `cr0_wp_change_under_paging_still_flushes`,
//!    `mov_cr3_still_flushes`) -- green before AND after, and they are what catches an inverted
//!    predicate, or an `old_cr0` captured after the assignment instead of before it. They do NOT
//!    catch a swapped old/new argument pair, which is provably inert; see the mutation ledger at
//!    the foot of this file;
//! 3. the LOAD-BEARING row (`retained_real_mode_chain_is_unreachable_under_pe`) -- green before
//!    and after, pinning the section 3.2 argument the whole slice rests on: a retained real-mode
//!    chain is not reachable under PE because `jit_mode_key` bit 1 is inside both `BlockKey` and
//!    `LinkTarget`.

use super::*;

/// A real-mode CPU with a 32-bit default code size, CS/DS/SS/ES at base 0. PE is CLEAR, which is
/// the precondition every keep row needs: the predicate must return `false` for these writes.
///
/// EVERY segment gets `default_size_32 = true`, not just CS. That is load-bearing for
/// `retained_real_mode_chain_is_unreachable_under_pe`: `jit_mode_key` carries the SS B bit as well
/// as PE, so a fixture that entered protected mode with a 32-bit SS after starting with a 16-bit
/// one would see the mode key move for a reason that has nothing to do with PE, and would pass
/// with PE dropped from the key entirely.
fn real_mode_cr0_cpu(entry: u32) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    for segment in [
        SegmentIndex::Cs,
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
    ] {
        cpu.load_segment_real(segment, 0);
        let mut descriptor = cpu.registers.segment(segment);
        descriptor.default_size_32 = true;
        cpu.registers.set_segment(segment, descriptor);
    }
    let mut cs = cpu.registers.cs();
    cs.access = 0x9b;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.set_eip(entry);
    cpu
}

/// `mov cr<n>, eax` (0f 22 /r, register form).
fn mov_to_cr(n: u8) -> [u8; 3] {
    [0x0f, 0x22, 0xc0 | (n << 3)]
}

/// `lmsw ax` (0f 01 /6, register form).
const LMSW_AX: [u8; 3] = [0x0f, 0x01, 0xf0];

/// Identity page tables covering the first 32 KB, so a PG-set arm can keep executing.
///
/// Directory at `0x8000`, one page table at `0x9000`. Without this the PG rows could only be
/// written as "set PG and stop", which would leave `cr0_pg_clear_still_flushes` unable to fetch
/// its own instruction.
const PAGE_DIRECTORY: u32 = 0x8000;
fn plant_identity_paging(memory: &mut [u8]) {
    memory[PAGE_DIRECTORY as usize..PAGE_DIRECTORY as usize + 4]
        .copy_from_slice(&0x0000_9003u32.to_le_bytes());
    for page in 0..16usize {
        let entry = ((page as u32) << 12) | 3;
        let at = 0x9000 + page * 4;
        memory[at..at + 4].copy_from_slice(&entry.to_le_bytes());
    }
}

/// Where the witness decode line lives, and where the CR0 writer lives. Different decode slots.
const WITNESS: u32 = 0x0400;
const WRITER: u32 = 0x0200;

fn cr0_fixture(writer: &[u8]) -> (CpuGsw, TestBus) {
    let mut memory = vec![0u8; 0x10000];
    memory[WRITER as usize..WRITER as usize + writer.len()].copy_from_slice(writer);
    // The witness: a NOP whose decode line is what the flush would kill.
    memory[WITNESS as usize] = 0x90;
    plant_identity_paging(&mut memory);
    let cpu = real_mode_cr0_cpu(WRITER);
    (cpu, TestBus::with_memory(memory))
}

/// Warm the witness line and prove it is live, so a later `line_live` assertion is not vacuous.
fn warm_witness(cpu: &mut CpuGsw, bus: &mut TestBus) {
    let d = cpu.registers.cs().default_size_32;
    cpu.fetch_decoded(bus, WITNESS).expect("witness decodes");
    assert!(
        cpu.decode_cache.line_live(WITNESS, d),
        "the witness line must be live before the CR0 write, or the row proves nothing"
    );
}

/// Execute exactly one instruction at `WRITER` with `eax = value`.
fn write_cr0(cpu: &mut CpuGsw, bus: &mut TestBus, value: u32) {
    cpu.registers.set_eax(value);
    cpu.set_eip(WRITER);
    exec_one_split(cpu, bus).expect("the control-register write must retire");
}

// ---- 1. the decode cache survives a PE toggle --------------------------------------------------

/// RED before the slice: each of the two writes bumps `decode_inval_other` and kills the line.
#[test]
fn cr0_pe_toggle_keeps_the_decode_cache() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    // Enter protected mode.
    write_cr0(&mut cpu, &mut bus, CR0_PE);
    assert_eq!(cpu.control.cr0 & CR0_PE, CR0_PE, "PE must actually be set");
    assert!(
        cpu.decode_cache.line_live(WITNESS, d),
        "setting PE moves no translation: the decode line must survive"
    );

    // And back out of it.
    write_cr0(&mut cpu, &mut bus, 0);
    assert_eq!(cpu.control.cr0 & CR0_PE, 0, "PE must actually be cleared");
    assert!(
        cpu.decode_cache.line_live(WITNESS, d),
        "clearing PE moves no translation either"
    );

    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        invalidations,
        "neither PE write may invalidate the code-translation caches"
    );
    assert_eq!(
        cpu.perf_counters().decode_inval_smc,
        0,
        "nothing here is self-modifying code"
    );
}

/// The O(1) fetch-window drop is NOT part of what the keep variant gives up: the eip-window
/// prefetch may hold bytes fetched under the old segmentation. Mutation row
/// "`flush_tlb_keep_code_caches` also drops the prefetch -> not dropped".
#[test]
fn pe_toggle_still_drops_prefetch() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    warm_witness(&mut cpu, &mut bus);

    write_cr0(&mut cpu, &mut bus, CR0_PE);

    assert_eq!(
        cpu.prefetch.len, 0,
        "the eip-window prefetch must be dropped across a mode change"
    );
    assert!(
        !cpu.code_page.valid,
        "the code-page translation cache must be dropped across a mode change"
    );
}

// ---- 3/4/6. the anti-regression rows -----------------------------------------------------------

/// GREEN before and after. Can-it-fail control: hard-code the predicate to `false` (design
/// section 7 item 3) and this row goes red.
#[test]
fn cr0_pg_set_still_flushes() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    // PG cannot be set with PE clear (#GP(0)); start already in protected mode.
    cpu.control.cr0 |= CR0_PE;
    cpu.control.cr3 = PAGE_DIRECTORY;
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    write_cr0(&mut cpu, &mut bus, CR0_PE | CR0_PG);

    assert_eq!(cpu.control.cr0 & CR0_PG, CR0_PG);
    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        invalidations + 1,
        "turning paging ON moves the map and must flush"
    );
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "the witness line must die when the map moves"
    );
}

/// The asymmetric partner of the row above: it is the one that separates `delta & PG` from
/// `new & PG`, and (with `cr0_wp_change_under_paging_still_flushes`) an old/new argument swap at
/// the MOV CR0 site.
#[test]
fn cr0_pg_clear_still_flushes() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = PAGE_DIRECTORY;
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    write_cr0(&mut cpu, &mut bus, CR0_PE);

    assert_eq!(cpu.control.cr0 & CR0_PG, 0);
    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        invalidations + 1,
        "turning paging OFF moves the map and must flush"
    );
    assert!(
        !cpu.decode_cache.line_live(WITNESS, d),
        "the witness line must die when the map moves"
    );
}

/// The second disjunct. Can-it-fail control: drop the WP term from the predicate.
#[test]
fn cr0_wp_change_under_paging_still_flushes() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = PAGE_DIRECTORY;
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    write_cr0(&mut cpu, &mut bus, CR0_PE | CR0_PG | CR0_WP);

    assert_eq!(cpu.control.cr0 & CR0_WP, CR0_WP);
    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        invalidations + 1,
        "a WP change while paging is on must flush"
    );
    assert!(!cpu.decode_cache.line_live(WITNESS, d));
}

/// A WP change with paging OFF is inert for translation, so it takes the keep path. This is the
/// row that makes the WP disjunct's `new & CR0_PG != 0` guard load-bearing rather than decorative.
#[test]
fn cr0_wp_change_without_paging_keeps_the_decode_cache() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(0));
    cpu.control.cr0 |= CR0_PE;
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    write_cr0(&mut cpu, &mut bus, CR0_PE | CR0_WP);

    assert_eq!(cpu.control.cr0 & CR0_WP, CR0_WP);
    assert_eq!(cpu.perf_counters().decode_inval_other, invalidations);
    assert!(cpu.decode_cache.line_live(WITNESS, d));
}

/// This slice does NOT touch the CR3 path. Mutation row "gate applied to the CR3 path too".
#[test]
fn mov_cr3_still_flushes() {
    let (mut cpu, mut bus) = cr0_fixture(&mov_to_cr(3));
    warm_witness(&mut cpu, &mut bus);
    let d = cpu.registers.cs().default_size_32;
    let invalidations = cpu.perf_counters().decode_inval_other;

    write_cr0(&mut cpu, &mut bus, PAGE_DIRECTORY);

    assert_eq!(cpu.control.cr3, PAGE_DIRECTORY);
    assert_eq!(
        cpu.perf_counters().decode_inval_other,
        invalidations + 1,
        "MOV CR3 is untouched by this slice and must still flush"
    );
    assert!(!cpu.decode_cache.line_live(WITNESS, d));
}

// ---- 5. LMSW, for every operand ----------------------------------------------------------------

/// LMSW's switchable set is MP|EM|TS, plus an optional PE SET. It can therefore never change PG
/// or WP, so the predicate is provably `false` at that site for EVERY operand -- which is also
/// why the old/new argument swap there is inert rather than caught (design section 8).
///
/// RED before the slice: every combination flushes.
#[test]
fn lmsw_never_flushes_for_any_operand() {
    for msw in 1u32..=0x0f {
        let (mut cpu, mut bus) = cr0_fixture(&LMSW_AX);
        warm_witness(&mut cpu, &mut bus);
        let d = cpu.registers.cs().default_size_32;
        let invalidations = cpu.perf_counters().decode_inval_other;

        write_cr0(&mut cpu, &mut bus, msw);

        assert_ne!(
            cpu.control.cr0, 0,
            "operand {msw:#x} must actually change CR0, or the row is vacuous"
        );
        assert_eq!(
            cpu.control.cr0 & (CR0_PG | CR0_WP),
            0,
            "LMSW can never reach PG or WP"
        );
        assert_eq!(
            cpu.perf_counters().decode_inval_other,
            invalidations,
            "LMSW operand {msw:#x} must not flush the code-translation caches"
        );
        assert!(
            cpu.decode_cache.line_live(WITNESS, d),
            "LMSW operand {msw:#x} must leave the decode line live"
        );
    }
}

// ---- 8. a real-mode line served in protected mode, and still faulting --------------------------

/// The end-to-end row for the situation this slice CREATES: a decode line inserted in real mode
/// at linear `L`, served in protected mode at the same linear and the same `d`.
///
/// Privilege is enforced executor-side from the decoded form, so the reuse is sound; nothing
/// tested it. The `line_live` assertion plus the miss-counter assertion are what stop the row
/// passing vacuously through a re-decode.
///
/// RED before the slice: LMSW flushes, so the line is gone and the row's premise never holds.
#[test]
fn pmode_entry_reuses_a_real_mode_decode_line_and_still_faults() {
    // `mov cr0, eax`: decodes identically in real mode and protected mode, and is CPL-0-only.
    // Privilege is checked by the EXECUTOR from the decoded form, which is exactly the property
    // this row exists to pin.
    const PRIVILEGED: u32 = 0x0500;

    let mut memory = vec![0u8; 0x10000];
    memory[WRITER as usize..WRITER as usize + 3].copy_from_slice(&LMSW_AX);
    memory[PRIVILEGED as usize..PRIVILEGED as usize + 3].copy_from_slice(&mov_to_cr(0));
    let mut cpu = real_mode_cr0_cpu(WRITER);
    let mut bus = TestBus::with_memory(memory);
    let d = cpu.registers.cs().default_size_32;

    // Warm the line in REAL mode.
    cpu.fetch_decoded(&mut bus, PRIVILEGED)
        .expect("the privileged instruction decodes");
    assert!(cpu.decode_cache.line_live(PRIVILEGED, d));

    // Enter protected mode through LMSW.
    write_cr0(&mut cpu, &mut bus, CR0_PE);
    assert_eq!(cpu.control.cr0 & CR0_PE, CR0_PE);
    assert!(
        cpu.decode_cache.line_live(PRIVILEGED, d),
        "the real-mode line must survive the mode change for this row to mean anything"
    );

    // Same linear, same `d`, now at CPL 3.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0xfb,
            default_size_32: d,
        },
    );
    cpu.cpl = 3;
    cpu.set_eip(PRIVILEGED);
    let misses = cpu.perf_counters().decode_misses;

    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            })
        ),
        "a warm real-mode line served under CPL 3 must still raise #GP(0): {result:?}"
    );
    assert_eq!(
        cpu.perf_counters().decode_misses,
        misses,
        "the fault must come from the RETAINED line, not from a fresh decode"
    );
    assert!(cpu.decode_cache.line_live(PRIVILEGED, d));
}

// ---- 2 and 7. the Direct link graph ------------------------------------------------------------

#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
mod links {
    use super::*;

    const CHAIN_ENTRY: u32 = 0x0200;
    const CHAIN_SECOND: u32 = 0x0220;
    const CHAIN_DONE: u32 = 0x0240;
    const CHAIN_WRITER: u32 = 0x0300;

    /// `stalls.links_cleared[Flushed]`, read through the public snapshot.
    fn flushed_clears(cpu: &CpuGsw) -> u64 {
        cpu.direct_stall_snapshot()
            .links_cleared
            .iter()
            .find(|(label, _)| *label == "flushed")
            .expect("the flushed lane exists")
            .1
    }

    /// A real-mode (PE clear) two-block chain, compiled, installed and LINKED.
    fn linked_chain() -> (CpuGsw, TestBus, jit::direct::CompiledBlock) {
        let mut memory = vec![0u8; 0x10000];
        memory[CHAIN_ENTRY as usize..CHAIN_ENTRY as usize + 10].copy_from_slice(&[
            0xb8, 0x01, 0x00, 0x00, 0x00, // mov eax,1
            0xe9, 0x16, 0x00, 0x00, 0x00, // jmp CHAIN_SECOND
        ]);
        memory[CHAIN_SECOND as usize..CHAIN_SECOND as usize + 10].copy_from_slice(&[
            0xb9, 0x02, 0x00, 0x00, 0x00, // mov ecx,2
            0xe9, 0x16, 0x00, 0x00, 0x00, // jmp CHAIN_DONE
        ]);
        memory[CHAIN_DONE as usize] = 0xf4; // hlt: never compiled, the chain ENDS here
        memory[CHAIN_WRITER as usize..CHAIN_WRITER as usize + 3].copy_from_slice(&mov_to_cr(0));

        let mut cpu = real_mode_cr0_cpu(CHAIN_ENTRY);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;

        warm_chain(&mut cpu, &mut bus);

        let entry_block = install(&mut cpu, CHAIN_ENTRY);
        install(&mut cpu, CHAIN_SECOND);
        (cpu, bus, entry_block)
    }

    /// `compile` walks the DECODE CACHE, not the bus, so every line the walk will visit has to be
    /// warm before it runs. Re-warming is also what makes the row below readable on both arms.
    fn warm_chain(cpu: &mut CpuGsw, bus: &mut TestBus) {
        for start in [CHAIN_ENTRY, CHAIN_ENTRY + 5, CHAIN_SECOND, CHAIN_SECOND + 5] {
            cpu.set_eip(start);
            cpu.fetch_decoded(bus, start).expect("chain decodes");
        }
    }

    fn install(cpu: &mut CpuGsw, linear: u32) -> jit::direct::CompiledBlock {
        let d = cpu.registers.cs().default_size_32;
        cpu.set_eip(linear);
        let key = jit::direct::key_for(cpu, linear, d).expect("the entry has a key");
        // The cache admits a compilation only for a key it has already seen twice: the first
        // probe registers the sighting, the second asks for the compile.
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Interpret
        ));
        assert!(matches!(
            cpu.jit_direct.probe(key),
            jit::direct::BlockProbe::Compile
        ));
        let compilation = jit::direct::compile(cpu, linear, d).expect("the block compiles");
        let id = cpu.jit_direct.install(&compilation).expect("it installs");
        cpu.jit_direct.block(id).expect("installed block is live")
    }

    /// Run the chain from its entry block and report whether the two blocks transferred NATIVELY
    /// (a bound link) rather than through the dispatcher.
    fn chain_runs_linked(
        cpu: &mut CpuGsw,
        bus: &mut TestBus,
        block: jit::direct::CompiledBlock,
    ) -> bool {
        cpu.set_eip(CHAIN_ENTRY);
        let before = cpu.perf_counters().jit_direct_linked_transfers;
        assert!(
            cpu.try_run_direct_block_for_test(bus, block).unwrap(),
            "the entry block must be admitted"
        );
        assert_eq!(
            cpu.registers.eip, CHAIN_DONE,
            "the chain must run to its end"
        );
        cpu.perf_counters().jit_direct_linked_transfers > before
    }

    fn write_chain_cr0(cpu: &mut CpuGsw, bus: &mut TestBus, value: u32) {
        cpu.registers.set_eax(value);
        cpu.set_eip(CHAIN_WRITER);
        exec_one_split(cpu, bus).expect("the CR0 write must retire");
    }

    /// RED before the slice: each CR0 write tears down the edge and bumps the `flushed` lane.
    #[test]
    fn cr0_pe_toggle_keeps_direct_links() {
        let (mut cpu, mut bus, entry_block) = linked_chain();
        assert!(
            chain_runs_linked(&mut cpu, &mut bus, entry_block),
            "the fixture must bind the edge, or the row proves nothing"
        );
        let cleared = flushed_clears(&cpu);
        let blocks = cpu.jit_direct.len();

        write_chain_cr0(&mut cpu, &mut bus, CR0_PE);
        write_chain_cr0(&mut cpu, &mut bus, 0);

        assert_eq!(
            flushed_clears(&cpu),
            cleared,
            "a PE toggle must not tear down any link"
        );
        assert_eq!(
            cpu.jit_direct.len(),
            blocks,
            "a PE toggle must not retire any block either"
        );
        assert!(
            chain_runs_linked(&mut cpu, &mut bus, entry_block),
            "the edge must still be bound after the round trip"
        );
    }

    /// THE LOAD-BEARING ROW. A retained real-mode chain must be UNREACHABLE once PE is set, even
    /// when the same linear address is addressable at the same `d` under a protected-mode
    /// selector. `jit_mode_key` bit 1 is PE; it is inside `BlockKey` (root probe) and inside
    /// `LinkTarget` (every edge), so no static edge can cross the boundary.
    ///
    /// Can-it-fail control: mask bit 1 out of `jit_mode_key` and this row goes red.
    #[test]
    fn retained_real_mode_chain_is_unreachable_under_pe() {
        let (mut cpu, mut bus, entry_block) = linked_chain();
        assert!(chain_runs_linked(&mut cpu, &mut bus, entry_block));
        let d = cpu.registers.cs().default_size_32;
        let real_key = jit::direct::key_for(&cpu, CHAIN_ENTRY, d).expect("real-mode key");
        assert!(matches!(
            cpu.jit_direct.probe(real_key),
            jit::direct::BlockProbe::Ready(_)
        ));
        let blocks = cpu.jit_direct.len();

        // Enter protected mode with a flat code selector: the SAME linear address at the SAME
        // `d`. This is what makes the row non-vacuous -- the linear address genuinely collides.
        write_chain_cr0(&mut cpu, &mut bus, CR0_PE);
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0008,
                base: 0,
                limit: 0xffff_ffff,
                access: 0x9b,
                default_size_32: d,
            },
        );
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

        // Re-warmed here so the row reads the same on both arms: before the slice the CR0 write
        // flushed the decode cache, and both `key_for` and the compile walk read it.
        warm_chain(&mut cpu, &mut bus);
        let pmode_key = jit::direct::key_for(&cpu, CHAIN_ENTRY, d).expect("pmode key");
        assert_eq!(
            pmode_key.linear, real_key.linear,
            "the two keys must share a linear address, or nothing collides"
        );
        assert_eq!(
            pmode_key.physical, real_key.physical,
            "and the same physical address: paging is off on both sides"
        );
        // Not merely "different": different in EXACTLY the PE bit. `jit_mode_key` also carries
        // CS.D, V86, the SS B bit and PG, and a fixture that let any of those move would stay
        // green with PE dropped from the key altogether -- which is the mutation this row is the
        // only catcher for.
        assert_eq!(
            pmode_key.mode_key ^ real_key.mode_key,
            1 << 1,
            "the two keys must differ in the PE bit and nothing else"
        );
        assert!(
            matches!(
                cpu.jit_direct.probe(pmode_key),
                jit::direct::BlockProbe::Interpret
            ),
            "the retained real-mode block must NOT be reachable under PE: a probe on the \
             protected-mode key must MISS, not land in the retained chain"
        );

        // And the pmode side gets a block of its own rather than inheriting one.
        cpu.set_eip(CHAIN_ENTRY);
        assert!(matches!(
            cpu.jit_direct.probe(pmode_key),
            jit::direct::BlockProbe::Compile
        ));
        let compilation =
            jit::direct::compile(&mut cpu, CHAIN_ENTRY, d).expect("the pmode block compiles");
        assert_eq!(compilation.span.key.mode_key, pmode_key.mode_key);
        cpu.jit_direct
            .install(&compilation)
            .expect("the pmode block installs");
        assert_eq!(
            cpu.jit_direct.len(),
            blocks + 1,
            "a NEW BlockKey must be installed for the protected-mode side"
        );
    }
}

// MUTATION EVIDENCE for the CR0 flush predicate (2026-08-29, applied by hand, run, restored).
// Each row names the fixture that caught it; a mutation nobody catches is recorded as a NON-GATE
// rather than claimed as coverage (`gates-that-cannot-fail-are-systemic`). Rows run against
// `cargo test -p izarravm-cpu --lib cr0_flush` unless the row says otherwise.
//
// | mutation | caught by |
// |---|---|
// | `cr0_write_moves_code_translation` -> always `true` | the five keep rows, and the split is EXACTLY the 6-pass/5-fail split this branch's first commit recorded against `0333d956` -- which is the design's requirement that the one-line kill switch be behaviour-identical to base, measured rather than assumed |
// | -> always `false` | `cr0_pg_set_still_flushes`, `cr0_pg_clear_still_flushes`, `cr0_wp_change_under_paging_still_flushes` |
// | `delta & CR0_PG` -> `new & CR0_PG` | `cr0_pg_clear_still_flushes` |
// | `delta & CR0_PG` -> `old & CR0_PG` | `cr0_pg_set_still_flushes` |
// | drop the WP disjunct | `cr0_wp_change_under_paging_still_flushes` |
// | `CR0_PG` -> `CR0_PE` in the predicate | seven rows: both PE-keep rows, the link row, LMSW, the pmode-reuse row, and both PG rows |
// | `old_cr0` captured AFTER the assignment at MOV CR0 | `cr0_pg_set_still_flushes`, `cr0_pg_clear_still_flushes`, `cr0_wp_change_under_paging_still_flushes` |
// | `old_cr0` captured AFTER the assignment at LMSW | **NON-GATE** -- inert, see below |
// | old/new arguments swapped at MOV CR0 | **NON-GATE** -- inert, see below |
// | old/new arguments swapped at LMSW | **NON-GATE** -- inert, see below |
// | drop PE from `jit_mode_key` | `retained_real_mode_chain_is_unreachable_under_pe` |
// | `BlockKey::new` keeps only `linear` | `retained_real_mode_chain_is_unreachable_under_pe`, `cr0_pe_toggle_keeps_direct_links` |
// | decode-line hit test stops comparing `d` (whole-crate run) | `d_bit_change_at_the_same_linear_address_re_decodes`, `decode_cache_hits_only_on_matching_tag_and_generation` -- NOT by any row here, because every row in this file uses one `d` on both sides of the mode change |
// | `flush_tlb_keep_code_caches` stops dropping the fetch frontend | `pe_toggle_still_drops_prefetch` |
// | the gate applied to the MOV CR3 path too | `mov_cr3_still_flushes` |
//
// THE ARGUMENT SWAP IS INERT AT BOTH SITES, and this corrects the design's expectation that the
// MOV CR0 swap would be caught by the PG rows. It is not, and it cannot be: `delta = old ^ new`
// is symmetric, and the only asymmetric term (`new & CR0_PG`) is reached only when
// `delta & CR0_PG == 0` -- at which point `old & CR0_PG == new & CR0_PG` by construction. So
// swapping the pair cannot change the predicate's answer for ANY input, at either site. The
// ordering hazard the design was actually worried about is a capture taken AFTER the assignment,
// which collapses both arguments to the new value; that one is real, it is caught at MOV CR0 by
// three rows, and it is inert at LMSW for the same reason every other LMSW mutation is (the
// predicate is false there for every operand).
//
// `drop PE from jit_mode_key` did NOT fail on the first attempt, and the reason is worth the
// ledger space: `real_mode_cr0_cpu` originally left SS with `default_size_32 = false`, so entering
// protected mode with a flat 32-bit SS moved `jit_mode_key` bit 3 as well as bit 1. The row's
// `assert_ne!` on the whole mode key was therefore satisfied by the SS bit alone and the mutation
// ran green. The fixture now gives every segment a 32-bit default and the row asserts the two keys
// differ in EXACTLY `1 << 1` -- an assertion that is stronger than the design asked for, and that
// is what turns this row into the only catcher the mutation has.
