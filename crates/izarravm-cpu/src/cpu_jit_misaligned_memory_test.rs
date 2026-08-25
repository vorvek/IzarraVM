// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Guard 3: the MISALIGNED memory admission at the two lean one-lookup sites.
//!
//! Before this slice `emit_wide_page_guard` refused every access whose address was not a multiple
//! of its `alignment_bytes()`, and on the payload fixture that single `jnz` was 99.97% of all
//! native side exits. The guard is now two independent halves: the page-CROSSING bound, which
//! still refuses at all thirteen call sites, and the ALIGNMENT test, which at the lean load and
//! store sites targets the site's own slow stub instead of a side exit.
//!
//! What each row here is for:
//!
//! | row | what would break it |
//! |---|---|
//! | misaligned Word/Dword reads run natively | the relaxation not landing, or landing at the wrong half |
//! | page-edge IN page runs, page-edge CROSSING exits | the crossing bound being relaxed along with the alignment test |
//! | crossing AND misaligned exits | the two halves emitted in the wrong ORDER -- with alignment first, a crossing access reaches the recovery stub and is served across a page boundary its FastMap entry does not cover |
//! | Mode 13h misaligned exits | the aperture refusal being dropped from the stub, or placed after the access |
//! | non-relaxed sites still exit | a site being relaxed that this slice does not touch |
//! | the split charge | the deposit being omitted, or sized `bytes()` instead of `bytes() - 1` |
//!
//! **The bus-clock comparison against the interpreter is NOT a plain equality here, and the reason
//! is a `TestBus` modelling artifact rather than a divergence.** The design's charge-equality claim
//! -- a misaligned N-byte access costs N byte cycles natively and interpreted alike -- is a
//! property of `MachineBus`, where `BusCycle::clocks_for` ignores width. `TestBus`'s direct-page
//! wait states are width-DEPENDENT (0/1/3 for Byte/Word/Dword), so the one wide cycle the native
//! side charges is not the same size as the byte cycle the interpreted side charges in its place.
//!
//! Both sides now emit `N - 1` byte cycles for the split -- native from the stub deposit,
//! interpreted from `charge_direct_ram_split`, since `FastMap::lookup_access` no longer refuses a
//! misaligned width -- so those terms cancel and the whole residual is `wide_cycle - byte_cycle`.
//! On a real `MachineBus` that residual is exactly ZERO, which is the charge equality itself,
//! asserted at the dial level in `izarravm-machine`'s `machine_bus_timing_test.rs`.
//!
//! So the rows below assert that exact residual, which is sharper than an equality would be: the
//! deposit is matched term-for-term by the interpreter's own split, so an omitted, doubled or
//! mis-sized deposit moves the number immediately.
//!
//! Everything else -- registers, lazy flags, EFLAGS, the halt latch, core clocks and the WHOLE of
//! guest RAM -- is still compared against a block-free interpreted role, and the whole-array RAM
//! compare is what catches a wrong-value store.

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry. An opcode at
/// a block's entry slot parks the block on the interpreter, so an entry-position fixture would
/// certify nothing.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The operand page: its own page, far from the code at `ENTRY` and the stack at `STACK_TOP`.
const OPERAND_PAGE: u32 = 0x5000;
/// The page above it, so a page-CROSSING access has real memory on the far side and the fixture
/// separates "refused because it crosses" from "faulted because nothing is there".
const NEXT_PAGE: u32 = 0x6000;
/// The canonical Mode 13h aperture base. `fast_map` classifies a page as `Mode13` purely by
/// physical range, so mapping this page is all it takes to build the aperture case.
const MODE13_PAGE: u32 = 0x000a_0000;

/// Big enough to contain the Mode 13h aperture, so `MODE13_PAGE` is real storage.
const MEMORY_LEN: usize = 0x000b_0000;

/// TestBus's direct-page wait states, restated here so the expected deltas below are derived
/// rather than copied. Keep in step with `TestBus::direct_page_wait_states`.
fn direct_cycle_clocks(bytes: u32) -> u64 {
    match bytes {
        1 => 2,
        2 => 3,
        4 => 5,
        other => unreachable!("no TestBus dial for a {other}-byte access"),
    }
}

/// What a natively-served misaligned `bytes`-wide access costs OVER the interpreted role, at
/// `TestBus`'s dials.
///
/// Derived, not measured, and the derivation is the interesting part:
///
/// * **Native** charges one WIDE cycle — the one the block's static count already carries — plus
///   `bytes - 1` byte cycles from the split deposit.
/// * **Interpreted** now charges `bytes` BYTE cycles. `FastMap::lookup_access` no longer has an
///   alignment rung, so a misaligned page-local access is SERVED from the fast map rather than
///   leaving it, and the charge routes to `CpuBus::charge_direct_ram_split` — whose default
///   implementation is `bytes` calls to `charge_direct_ram_memory` at `BusWidth::Byte`, which is
///   what `TestBus` inherits.
///
/// So the difference collapses to `wide_cycle(bytes) - byte_cycle`: the `bytes - 1` byte cycles
/// appear on BOTH sides and cancel. **Everything that remains is `TestBus`'s width-DEPENDENT
/// direct dial** (0/1/3 wait states for Byte/Word/Dword). On a real `MachineBus`, where
/// `BusCycle::clocks_for` ignores width, this quantity is exactly ZERO — which is the charge
/// equality this slice rests on, asserted directly in `machine_bus_timing_test.rs`.
///
/// That makes the residual here a pure fixture artifact rather than a divergence, and it is a
/// sharper assertion than it was before the interpreter slice landed: the deposit is now matched
/// term-for-term by the interpreter's own split, so an omitted, doubled or mis-sized deposit moves
/// this number immediately.
fn expected_split_delta(bytes: u32) -> u64 {
    direct_cycle_clocks(bytes) - direct_cycle_clocks(1)
}

/// A distinct byte at every address, so a store of the wrong WIDTH or the wrong VALUE changes
/// guest RAM even when it writes plausible bytes in the right place.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; MEMORY_LEN];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    /// Linear address of the tested opcode, i.e. where a guarded exit must leave EIP.
    body_at: u32,
}

/// Map one page for read and write on the fast map. A memory-form slot silently never compiles
/// without this, and the fixture would then certify a refusal it did not intend to test.
fn map_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    for write in [false, true] {
        let kind = if write {
            BusAccessKind::DataWrite
        } else {
            BusAccessKind::DataRead
        };
        let host = bus.direct_page(page, kind).unwrap().unwrap();
        let ok = if write {
            cpu.jit_fast_map.populate_write(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        } else {
            cpu.jit_fast_map.populate_read(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        };
        assert!(ok, "page {page:#x} must map");
    }
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
fn build(body: &[u8]) -> Roles {
    build_watching(body, None)
}

/// As `build`, but marks one byte as code on BOTH roles before the pages are mapped.
///
/// Marking makes the page's PAGE_WATCHED bit set, which POISONS its store bias -- so an aligned
/// store to any other offset on that page reaches `emit_slow_stub` instead of the site's fast arm,
/// passes the code-watch guard (its own granule is unmarked), and lands through the stub's RAM
/// counter arm. That is the only way to build an aligned store that reaches the stub, and it is
/// what the unconditional-deposit row needs.
///
/// The mark goes on BOTH roles: the watch is guest-visible through the interpreter's own SMC
/// bookkeeping, so watching one role would compare two different machines. It goes BEFORE
/// `map_page`, because the mark's edge sweep invalidates any live fast-map entry on the marked
/// page whose watched bit is clear -- marking after populating would clear the entry just
/// installed.
fn build_watching(body: &[u8], watch: Option<u32>) -> Roles {
    build_slots(&[body], watch)
}

/// As `build`, for a pair of tested opcodes rather than one.
fn build_two_body(first: &[u8], second: &[u8]) -> Roles {
    build_slots(&[first, second], None)
}

/// Each element of `bodies` is one tested opcode, and each gets its own warmed decode line: a
/// missing one makes `compile` ask for a retry rather than produce a block. The compiled block
/// must then cover `bodies.len() + 2` slots -- asserted rather than assumed, because a body that
/// silently failed to compile as one block would leave the tested opcode on the interpreter and
/// the fixture would certify nothing.
fn build_slots(bodies: &[&[u8]], watch: Option<u32>) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let mut starts = vec![ENTRY];
    let body_at = ENTRY + code.len() as u32;
    for body in bodies {
        starts.push(ENTRY + code.len() as u32);
        code.extend_from_slice(body);
    }
    starts.push(ENTRY + code.len() as u32);
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);
    let slots = (bodies.len() + 2) as u8;

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut native = flat_cpu();
    let mut interp = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        if let Some(at) = watch {
            cpu.mark_decode_code_for_test(at, 1);
        }
        for page in [
            OPERAND_PAGE,
            NEXT_PAGE,
            MODE13_PAGE,
            (STACK_TOP - 4) & !0xfff,
        ] {
            map_page(cpu, bus, page);
        }
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the tested form never reached the memory guard")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, slots,
        "the block must cover every slot, so the tested opcode really ran natively"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        // 0xdead in every high half; the low half is the register index, so a row that reads or
        // writes the wrong register is a distinguishable failure rather than a coincidence.
        cpu.registers.gpr = std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32));
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = 0x202;
        cpu.pending_flags = PendingFlags::default();
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
        body_at,
    }
}

/// Everything except bus clocks. See the module header for why the bus-clock axis is asserted as
/// an exact delta instead of an equality here.
fn compare_state(roles: &Roles, context: &str) {
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: registers"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: EFLAGS"
    );
    assert_eq!(
        roles.native.halted, roles.interp.halted,
        "{context}: halt latch"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "{context}: core clocks"
    );
    // The whole array, not a window around the operand. A store that widened, or that wrote a
    // STALE value, touches bytes the interpreter did not, and a window sized to the intended
    // access is exactly the wrong shape to see either.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY through the misaligned recovery: all three slots retire in the
/// block, the architectural state matches three interpreted steps, and the bus-clock delta is
/// exactly the split charge for `bytes` -- one wide cycle plus `bytes - 1` byte cycles.
fn lowered_misaligned(body: &[u8], bytes: u32, context: &str) {
    let mut roles = build(body);
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        3,
        "{context}: all three slots must retire natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
    // Additive rather than a subtraction of the two totals: a mutation that DROPS the deposit
    // makes the native role cheaper than the interpreted one, and `u64` subtraction would panic on
    // the underflow instead of reporting the two numbers.
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks() + expected_split_delta(bytes),
        "{context}: native must charge one wide cycle plus {} byte cycles where the interpreter          charges {bytes} byte cycles",
        bytes - 1
    );
}

/// A row that completes NATIVELY with no misaligned access at all: the aligned control. Bus clocks
/// must match the interpreter EXACTLY here -- an aligned access takes the same one direct cycle in
/// both roles -- which is what says the split deposit is conditional rather than unconditional.
fn lowered_aligned(body: &[u8], context: &str) {
    let mut roles = build(body);
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: an ALIGNED access must charge exactly what it charged before the slice"
    );
}

/// A row whose emitted memory guard REFUSES. The native run must end at the tested opcode with the
/// instruction un-started -- no byte of guest RAM written, no register touched -- and the
/// interpreter must then execute it and reach the same state.
fn guarded(body: &[u8], exits: fn(&CpuGsw) -> u64, context: &str) {
    let mut roles = build(body);
    let retired = roles.native.perf_counters().jit_direct_insns;
    let before = exits(&roles.native);
    let ram_before = roles.native_bus.memory.to_vec();
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        1,
        "{context}: only the slot BEFORE the tested opcode may retire natively"
    );
    assert_eq!(
        exits(&roles.native) - before,
        1,
        "{context}: exactly one side exit of the expected reason"
    );
    assert_eq!(
        roles.native.registers.eip, roles.body_at,
        "{context}: the run must end AT the tested opcode, not after it"
    );
    // The refusal must be TRANSACTIONAL: not one byte of the access may have landed. This is the
    // assertion that separates "refused before the access" from "refused after it", and it is the
    // whole content of the Mode 13h store row -- an aperture byte written before the refusal would
    // be written a second time by the interpreter's re-execution.
    // Reported as the first differing index rather than as two 700 KiB arrays: a refusal that
    // wrote before exiting differs in one or two bytes, and the address is the diagnosis.
    let touched = roles
        .native_bus
        .memory
        .iter()
        .zip(ram_before.iter())
        .position(|(now, before)| now != before);
    assert_eq!(
        touched, None,
        "{context}: the refusal must leave guest RAM untouched, but byte {touched:?} moved -- a          refusal placed AFTER the write means the interpreter's re-execution writes it twice"
    );
    roles.interp.cycle(&mut roles.interp_bus).unwrap();
    compare_state(&roles, &format!("{context}: at the guard"));

    for _ in 0..2 {
        roles.native.cycle(&mut roles.native_bus).unwrap();
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, &format!("{context}: after the re-execution"));
}

fn alignment_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_cross_page_or_alignment
}

fn kind_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_unavailable_or_kind
}

/// `[disp32]` addressing: ModRM mod 00, rm 101. No base register, so no row's operand address can
/// depend on the poisoned register seeds.
fn disp32(opcode_head: &[u8], reg: u8, at: u32) -> Vec<u8> {
    let mut body = opcode_head.to_vec();
    body.push((reg << 3) | 0b101);
    body.extend_from_slice(&at.to_le_bytes());
    body
}

/// `movzx ebx, word [at]` — a Word READ through the lean one-lookup load site.
fn word_load(at: u32) -> Vec<u8> {
    disp32(&[0x0f, 0xb7], 3, at)
}

/// `mov ebx, dword [at]` — a Dword READ through the same site.
fn dword_load(at: u32) -> Vec<u8> {
    disp32(&[0x8b], 3, at)
}

// ---------------------------------------------------------------------------------------------
// The admission matrix: reads
// ---------------------------------------------------------------------------------------------

/// A misaligned Word read runs natively and charges two byte cycles' worth.
///
/// Every odd offset in a 16-byte window, so a row that happened to work at one alignment and not
/// another fails. The 386 admits misaligned accesses architecturally; before this slice the native
/// backend refused them all, which was a missed lowering rather than a divergence.
#[test]
fn a_misaligned_word_read_runs_natively_and_charges_the_split() {
    for offset in (1..0x20).step_by(2) {
        let at = OPERAND_PAGE + offset;
        lowered_misaligned(&word_load(at), 2, &format!("movzx ebx, word [{at:#x}]"));
    }
}

/// The sixteen-bit DESTINATION form of the same read, moved here from `word_memory` where it
/// asserted the old refusal.
///
/// It is a different lowering from the row above, not a duplicate: at Word operand size MOVZX
/// defines the destination's low 16 bits and PRESERVES its high 16, so the register seeds' `0xdead`
/// high halves are load-bearing. `word_memory` cannot host it any more -- that module's `lowered`
/// asserts bus clocks EQUAL to the interpreter, and a natively-served misaligned access charges
/// more than `TestBus`'s non-splitting slow path.
#[test]
fn a_misaligned_word_read_into_a_sixteen_bit_destination_runs_natively() {
    for offset in [1u32, 3, 0x11] {
        let at = OPERAND_PAGE + 0x400 + offset;
        let mut body = vec![0x66u8];
        body.extend_from_slice(&word_load(at));
        lowered_misaligned(&body, 2, &format!("movzx bx, word [{at:#x}]"));
    }
}

/// The sixteen-bit ALU memory-SOURCE form of the same read: `sub ax, word [odd]`.
///
/// `AluMemSource` reads through `emit_ram_read_pointer`, which dispatches to this slice's RELAXED
/// lean read site whenever `one_lookup_load` is on (the default). So the form-3 word-memory
/// admission does NOT convert a barrier into a per-execution side exit at misaligned addresses --
/// it is served natively with the split charge, exactly like the MOVZX rows above. That is the
/// economics claim the admission rests on, and it belongs here rather than in `word_memory`, whose
/// `lowered` asserts bus clocks EQUAL to the interpreter.
///
/// It is not a duplicate of the loads: this row also WRITES a sixteen-bit register back and
/// rewrites the lazy descriptor, so a lowering that served the misaligned read but mishandled the
/// tail still fails here on registers or lazy flags.
#[test]
fn a_misaligned_word_alu_memory_source_runs_natively() {
    for offset in [1u32, 3, 0x11] {
        let at = OPERAND_PAGE + 0x800 + offset;
        let mut body = vec![0x66u8];
        body.extend_from_slice(&disp32(&[0x2b], 0, at));
        lowered_misaligned(&body, 2, &format!("sub ax, word [{at:#x}]"));
    }

    // The other half of the split guard, on the same slot: an operand on the page's LAST byte
    // CROSSES, and the crossing bound refuses it whatever the alignment relaxation does. Without
    // this row the test above would keep passing if the crossing half were relaxed along with the
    // alignment half, and the pointer would then be used across a page the FastMap entry does not
    // cover. `guarded` also asserts the refusal is transactional -- EIP left AT the slot, guest RAM
    // untouched, and the interpreted re-execution agreeing on both roles.
    let at = OPERAND_PAGE + 0xfff;
    let mut body = vec![0x66u8];
    body.extend_from_slice(&disp32(&[0x2b], 0, at));
    guarded(
        &body,
        alignment_exits,
        &format!("sub ax, word [{at:#x}] crosses"),
    );
}

/// An ALIGNED read that reaches the counting read STUB must charge exactly what the inline fast
/// arm charges. The read-side twin of `an_aligned_store_through_the_slow_stub_charges_no_split`.
///
/// Reaching this state is the whole difficulty, and the route is narrower than it looks. Neither
/// of the two obvious candidates works: a supervisor-tagged page at cpl0 strips its tag and
/// rejoins the fast arm inline, never entering the stub, and a page with no committed read bias
/// hits `emit_read_pointer`'s `UNAVAILABLE_BIAS` arm and returns status 1 without reaching the
/// deposit.
///
/// What does work is a page whose HOST BACKING is not 4 KiB-aligned. `derive_load_bias` poisons
/// the LOAD bias when `read_bias & PAGE_MASK != 0` -- the load bias carries the mode13 and
/// supervisor tags in its low bits, so a bias with low bits of its own cannot be tagged -- while
/// `read_biases[index]` stays live and available. `FastMap::populate` does not require the pointer
/// to be page-aligned, so the site's probe sees poison and jumps to the stub, and the stub's
/// `emit_read_pointer` resolves the untagged read bias perfectly well. An aligned wide read then
/// lands on the deposit point.
///
/// The fixture installs a MIRROR of the operand page at a deliberately odd host address. Reads
/// resolve to `mirror + (linear - page_base)`, so seeding the mirror from the same fill keeps both
/// roles reading identical bytes; the page is only read here, so the mirror never has to be
/// written back.
#[test]
fn an_aligned_read_through_the_counting_stub_charges_no_split() {
    // Leaked on purpose: the fast map holds a raw pointer into this buffer for the life of the
    // test, and dropping it while an entry is live would dangle. One test's page is a fair trade
    // against threading a lifetime through the fixture.
    let mirror: &'static mut [u8] = Box::leak(vec![0u8; 0x2000].into_boxed_slice());
    let fill = memory_fill();
    // An ODD host address inside the buffer, so `bias = ptr - OPERAND_PAGE` has low bits set and
    // `derive_load_bias` poisons the load bias. The offset is otherwise arbitrary.
    let base = mirror.as_mut_ptr();
    let skew = 1usize;
    mirror[skew..skew + 0x1000]
        .copy_from_slice(&fill[OPERAND_PAGE as usize..OPERAND_PAGE as usize + 0x1000]);
    let ptr = unsafe { base.add(skew) };
    assert_ne!(
        (ptr as usize).wrapping_sub(OPERAND_PAGE as usize) & 0xfff,
        0,
        "the fixture's whole point is a bias with low bits set"
    );

    let at = OPERAND_PAGE + 0x120;
    let mut roles = build(&dword_load(at));
    // Re-populate the operand page's READ mapping over the mirror, on the native role only: the
    // interpreted role must keep reading `bus.memory`, which holds the same bytes.
    let ok = roles.native.jit_fast_map.populate_read(
        OPERAND_PAGE,
        OPERAND_PAGE,
        izarravm_bus::DirectPage {
            physical_page: OPERAND_PAGE,
            ptr,
            len: 0x1000,
            writable: false,
            mapping_epoch: roles.native_bus.direct_mapping_epoch,
        },
        jit::fast_map::PagePermissions::UNPAGED,
        roles.native.physical_page_watched(OPERAND_PAGE),
    );
    assert!(ok, "the skewed mirror must map");
    assert_eq!(
        roles.native.jit_fast_map.load_bias_for_test(OPERAND_PAGE),
        jit::fast_map::NATIVE_LOAD_BIAS_POISON,
        "a non-page-aligned host backing must poison the LOAD bias, or this fixture is testing \
         the inline fast arm instead of the stub"
    );

    let before = roles.native.perf_counters().jit_direct_side_exits;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_side_exits - before,
        0,
        "the stub must SERVE the read, not refuse it"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, "aligned read through the counting stub");
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "an ALIGNED read reaching the counting stub must charge exactly what the fast arm \
         charges; an unconditional split deposit over-charges every read on a page whose host \
         backing is not 4 KiB-aligned, permanently and silently"
    );
}

/// A misaligned Dword read runs natively at all three misalignments, and the ALIGNED one is the
/// control that says the deposit is conditional.
///
/// All three sub-alignments matter separately: `+1` and `+3` are odd, `+2` is even but not a
/// multiple of four, and an implementation that tested `al & 1` rather than
/// `al & (alignment_bytes() - 1)` would serve `+2` while charging it as aligned.
#[test]
fn a_misaligned_dword_read_runs_natively_at_every_sub_alignment() {
    let base = OPERAND_PAGE + 0x100;
    lowered_aligned(&dword_load(base), "mov ebx, dword [aligned]");
    for offset in 1..4 {
        let at = base + offset;
        lowered_misaligned(&dword_load(at), 4, &format!("mov ebx, dword [{at:#x}]"));
    }
}

/// The page edge, both sides of it, and this is the row the guard ORDER exists for.
///
/// An access that ends on the page's last byte is served; one that would run past it is refused,
/// because the FastMap entry the pointer was formed against covers exactly one page. The crossing
/// bound is emitted BEFORE the alignment test precisely so a crossing access can never reach the
/// recovery path the alignment test now targets — reverse the two halves and the last three rows
/// here are served across a page boundary instead of exiting.
#[test]
fn a_page_edge_access_runs_inside_the_page_and_exits_when_it_crosses() {
    // Word: 0xFFD and 0xFFE end inside the page (0xFFE is aligned, so it is the control); 0xFFF
    // crosses.
    lowered_misaligned(
        &word_load(OPERAND_PAGE + 0xffd),
        2,
        "movzx ebx, word [page+0xffd]",
    );
    lowered_aligned(&word_load(OPERAND_PAGE + 0xffe), "word [page+0xffe]");
    guarded(
        &word_load(OPERAND_PAGE + 0xfff),
        alignment_exits,
        "movzx ebx, word [page+0xfff] crosses",
    );

    // Dword: 0xFF9/0xFFA/0xFFB end inside; 0xFFD/0xFFE/0xFFF cross. 0xFFC is the aligned control.
    for offset in [0xff9u32, 0xffa, 0xffb] {
        let at = OPERAND_PAGE + offset;
        lowered_misaligned(&dword_load(at), 4, &format!("dword [{at:#x}] fits"));
    }
    lowered_aligned(&dword_load(OPERAND_PAGE + 0xffc), "dword [page+0xffc]");
    for offset in [0xffdu32, 0xffe, 0xfff] {
        let at = OPERAND_PAGE + offset;
        guarded(
            &dword_load(at),
            alignment_exits,
            &format!("dword [{at:#x}] crosses"),
        );
    }
}

/// A Mode 13h aperture read stays refused even when it is misaligned, and stays refused for the
/// aperture's sake rather than the alignment's.
///
/// The refusal lives at the counting read stub's mode13 tail, BEFORE the permission check, which
/// is where the pre-slice guard sat relative to every check: refusing later would re-attribute a
/// cpl3 aperture case from alignment to Permission.
///
/// The exit REASON is `UnavailableOrKind`, not `CrossPageOrAlignment`, and that is deliberate: a
/// dedicated status would add a compare and a branch to every read site's cold dispatch, roughly
/// ten per block, for an attribution difference over a population bounded near 0.01%.
#[test]
fn a_misaligned_mode13_read_still_exits() {
    for offset in [1u32, 3] {
        let at = MODE13_PAGE + 0x100 + offset;
        guarded(
            &word_load(at),
            kind_exits,
            &format!("movzx ebx, word [mode13+{offset}]"),
        );
    }
    let at = MODE13_PAGE + 0x101;
    guarded(&dword_load(at), kind_exits, "mov ebx, dword [mode13+1]");
}

// ---------------------------------------------------------------------------------------------
// The admission matrix: stores
// ---------------------------------------------------------------------------------------------

/// `mov word [at], imm16` — a Word STORE through the lean one-lookup store site.
fn word_store(at: u32, value: u16) -> Vec<u8> {
    let mut body = vec![0x66u8];
    body.extend_from_slice(&disp32(&[0xc7], 0, at));
    body.extend_from_slice(&value.to_le_bytes());
    body
}

/// `mov dword [at], imm32`.
fn dword_store(at: u32, value: u32) -> Vec<u8> {
    let mut body = disp32(&[0xc7], 0, at);
    body.extend_from_slice(&value.to_le_bytes());
    body
}

/// A misaligned Word or Dword store runs natively, writes the right bytes, and charges the split.
///
/// The whole-array guest-RAM compare in `compare_state` is what makes this a real assertion:
/// `memory_fill` puts a distinct byte at every address, so a store of the wrong WIDTH or to the
/// wrong ADDRESS changes bytes the interpreter left alone.
#[test]
fn a_misaligned_store_runs_natively_and_charges_the_split() {
    for offset in (1..0x10).step_by(2) {
        let at = OPERAND_PAGE + 0x600 + offset;
        lowered_misaligned(&word_store(at, 0x1234), 2, &format!("mov word [{at:#x}]"));
    }
    for offset in 1..4 {
        let at = OPERAND_PAGE + 0x700 + offset;
        lowered_misaligned(
            &dword_store(at, 0x1020_3040),
            4,
            &format!("mov dword [{at:#x}]"),
        );
    }
}

/// **The store VALUE must be the slot's own.** Two store slots, the second misaligned, the first
/// writing a distinguishable value.
///
/// This row exists for one specific wrong shape, and it was written and watched to FAIL against it
/// before the fix landed. The slow stub's contract is "RAX = linear address, RDX = store value",
/// and `emit_slow_stub` spills RDX as its second instruction. But `emit_store_fast` materialises
/// the value into RDX three emissions AFTER the point the guard sits. Retarget the alignment half
/// in place -- the obvious edit, and the one the read site takes -- and the jump into the stub
/// happens before the value exists: the stub spills, then stores, whatever RDX held from the
/// PREVIOUS slot. Silent wrong-value store into guest RAM, no fault, no counter.
///
/// Two slots with different values is the only shape where a stale RDX is visible: with one slot,
/// or with two slots writing the same value, the wrong register still holds a plausible number.
///
/// The fix splits the guard around the value materialisation and gives the alignment half RCX,
/// which is free by the store pad's own rule. This row is what guards that split against a future
/// tidy-up that hoists the alignment half back next to the crossing bound.
#[test]
fn a_misaligned_store_writes_its_own_value_not_the_previous_slots() {
    // The first slot is ALIGNED, so it takes the fast arm and leaves its value in RDX; the second
    // is misaligned and goes through the stub. Distinguishable values, and neither is the other's
    // truncation.
    let first = OPERAND_PAGE + 0x800;
    let second = OPERAND_PAGE + 0x901;
    // Four slots now, not three: the fixture's own leading and trailing moves plus two stores.
    let mut roles = build_two_body(&word_store(first, 0xa5a5), &word_store(second, 0x3c3c));
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        4,
        "all four slots must retire natively"
    );
    for _ in 0..4 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    // The whole-array compare is the primary assertion; the explicit read below names the failure
    // so a reader does not have to diff two 700 KiB arrays to see what went wrong.
    let stored = u16::from_le_bytes([
        roles.native_bus.memory[second as usize],
        roles.native_bus.memory[second as usize + 1],
    ]);
    assert_eq!(
        stored, 0x3c3c,
        "the misaligned store wrote {stored:#06x}: with the alignment half emitted BEFORE \
         `emit_read_store_value`, the slow stub spills and stores the PREVIOUS slot's value"
    );
    compare_state(&roles, "two stores, the second misaligned");
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks() + expected_split_delta(2),
        "exactly one of the two stores is misaligned"
    );
}

/// An ALIGNED store that reaches the SLOW STUB must charge exactly what the fast arm charges.
///
/// This is the row that keeps the stub's RAM deposit CONDITIONAL, and the population it protects
/// is not exotic: a watched page's store bias is poisoned, so every store to a watched page goes
/// through this stub, and watched pages are the hot store class on the self-modifying-code
/// fixtures. Poisoned and supervisor entries arrive the same way. An unconditional
/// `emit_dynamic_split_extra` in the stub's RAM counter arm would over-charge every one of them.
///
/// The page is watched at one byte; the store is at a different offset, so its own granule is
/// unmarked and the code-watch guard passes. Bus clocks must match the interpreter EXACTLY.
#[test]
fn an_aligned_store_through_the_slow_stub_charges_no_split() {
    let watched_byte = OPERAND_PAGE + 0xa00;
    let at = OPERAND_PAGE + 0xb00;
    let mut roles = build_watching(&word_store(at, 0x5aa5), Some(watched_byte));
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, "aligned store through the slow stub");
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "an ALIGNED store reaching the slow stub must charge exactly what the fast arm charges; \
         an unconditional split deposit over-charges every watched, poisoned and supervisor page"
    );
}

/// A misaligned Mode 13h aperture store stays refused, and -- the whole point of where the refusal
/// sits -- writes NO byte before refusing.
///
/// The read stub has separate mode13 and RAM tails; the slow STORE stub does not. Both kind arms
/// fall into a shared `store` label and the kind is re-split only AFTER the write, so a refusal
/// placed "in the mode13 tail" by analogy with the read side would land after the aperture byte
/// had been written -- and the block would then side-exit, so the interpreter would re-execute the
/// instruction and write it a SECOND time. The refusal therefore sits at the `mode13` label,
/// before the permission check, and `guarded`'s whole-RAM snapshot is what proves it.
#[test]
fn a_misaligned_mode13_store_still_exits_without_writing() {
    for offset in [1u32, 3] {
        let at = MODE13_PAGE + 0x200 + offset;
        guarded(
            &word_store(at, 0x1234),
            kind_exits,
            &format!("mov word [mode13+{offset}]"),
        );
    }
    guarded(
        &dword_store(MODE13_PAGE + 0x301, 0x1020_3040),
        kind_exits,
        "mov dword [mode13+1]",
    );
}

/// A page-CROSSING store still refuses, at both widths. The crossing bound is emitted before the
/// alignment half here for the same reason it is at the read site.
#[test]
fn a_page_crossing_store_still_exits() {
    guarded(
        &word_store(OPERAND_PAGE + 0xfff, 0x1234),
        alignment_exits,
        "mov word [page+0xfff] crosses",
    );
    for offset in [0xffdu32, 0xffe, 0xfff] {
        let at = OPERAND_PAGE + offset;
        guarded(
            &dword_store(at, 0x1020_3040),
            alignment_exits,
            &format!("mov dword [{at:#x}] crosses"),
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The code-watch granule span, at an offset only a misaligned store can reach
// ---------------------------------------------------------------------------------------------

/// A misaligned Dword store STRADDLING the mask's `u64` word boundary, with the watch bit on each
/// byte of its span in turn.
///
/// This row is the reason the emitter's granule span is computed as a WORST CASE over offsets
/// rather than as the span of an aligned access. Until the store site served misaligned accesses
/// there was no way to construct it: the alignment guard refused the store before the code-watch
/// guard could run, so every input the watch guard had ever seen was aligned, and a span formula
/// that only held for aligned accesses was indistinguishable from a correct one.
///
/// Page offset 63 puts the store's four bytes at 63, 64, 65, 66 -- straddling the mask word
/// boundary at bit 64 at the shipped one-byte granule, which is the guard's multi-granule window
/// arm on a misaligned input.
///
/// **Byte 66 is the discriminating row and the others are not, which is worth stating because two
/// of them fail for the wrong reason under the granule mutation:**
///
/// | watched byte | shipped shift 0 | under the mutation (shift 1, aligned-span `n = 2`) |
/// |---|---|---|
/// | **66** — the access's LAST byte | granule 66, tested → exits | granule **33**, which `n = 2` never tests → does NOT exit. **The discriminating row** |
/// | 65 | granule 65 → exits | granule 32, tested by both formulas → exits. A control, not evidence |
/// | 62 — must NOT exit | granule 62, outside the access → does not exit | granule 31, which byte 63 also occupies, so a WATCHED verdict is CORRECT at 2-byte granules → exits, red for the right reason at the wrong granularity |
///
/// The byte-62 row is therefore **SHIPPED-SHIFT-ONLY**: it cannot be made shift-independent,
/// because at any coarser granule some nearby non-member byte shares a member granule. Leaving it
/// unqualified would be worse than having no row at all — the suite would go red under the
/// mutation without the granule bug being the cause, and that reads as passing evidence.
#[test]
fn a_straddling_misaligned_store_sees_every_granule_of_its_span() {
    let base = OPERAND_PAGE + 63;
    // Each byte the store actually occupies must produce a watch exit. Byte 66 is the one the
    // aligned-span formula gets wrong at a coarser granule.
    for watched in [base, base + 1, base + 2, base + 3] {
        let mut roles = build_watching(&dword_store(base, 0x1020_3040), Some(watched));
        let before = roles.native.perf_counters().jit_direct_exit_code_watch;
        assert!(
            roles
                .native
                .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
                .unwrap(),
            "watched={watched:#x}: block did not run natively"
        );
        assert_eq!(
            roles.native.perf_counters().jit_direct_exit_code_watch - before,
            1,
            "watched={watched:#x}: a store spanning {base:#x}..={:#x} must see the watch bit on \
             EVERY byte of its span, including its last",
            base + 3
        );
    }

    // SHIPPED-SHIFT-ONLY. A byte outside the store's span must NOT be watched -- at ONE-BYTE
    // granules. At any coarser granule byte 62 shares a granule with byte 63, which the store does
    // occupy, so a watch exit here would be CORRECT rather than a defect. Do not read a failure of
    // this row under a granule-size change as evidence of anything.
    let mut roles = build_watching(&dword_store(base, 0x1020_3040), Some(OPERAND_PAGE + 62));
    let before = roles.native.perf_counters().jit_direct_exit_code_watch;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_exit_code_watch - before,
        0,
        "a byte outside the store's span must not be watched at one-byte granules"
    );
}

// ---------------------------------------------------------------------------------------------
// The sites this slice does NOT relax
// ---------------------------------------------------------------------------------------------

/// The eleven non-relaxed sites keep refusing every misaligned access, and two of them are checked
/// here rather than argued: the read-modify-write ALU destination and the memory INC.
///
/// They are not an oversight. An RMW slot needs a read deposit AND a write deposit inside one
/// slot, which is its own change; until then the whole guard refuses there, unchanged.
#[test]
fn the_non_relaxed_sites_still_refuse_a_misaligned_access() {
    let at = OPERAND_PAGE + 0x201;

    // `add dword [at], 0x01` -- `emit_alu_mem_dest`, the read-modify-write site.
    let mut alu = disp32(&[0x83], 0, at);
    alu.push(0x01);
    guarded(&alu, alignment_exits, "add dword [odd], imm8");

    // `inc dword [at]` -- `emit_rmw_inc_dec_dword`.
    let inc = disp32(&[0xff], 0, at);
    guarded(&inc, alignment_exits, "inc dword [odd]");
}

/// x87 stays refused at every misalignment. `Qword` and `Tbyte` deliberately ask for 4-byte
/// alignment rather than their own size, because the interpreter issues an m64 as two independently
/// 4-aligned dword transactions; admitting a 2-aligned m80 would diverge on bus timing rather than
/// on bytes. Nothing in this slice touches that, and this row says so.
#[test]
fn a_misaligned_x87_access_still_exits() {
    // `fld qword [at]` -- DD /0.
    for offset in [1u32, 2, 3] {
        let at = OPERAND_PAGE + 0x300 + offset;
        guarded(
            &disp32(&[0xdd], 0, at),
            alignment_exits,
            &format!("fld qword [+{offset}]"),
        );
    }
}
