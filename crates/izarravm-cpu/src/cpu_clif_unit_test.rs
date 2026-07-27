// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! C0 Task L: one register-only guest unit (`add eax, ebx`) hand-built in CLIF at the tail
//! convention, executed against a real `CpuGsw`, and compared against the interpreter running
//! the equivalent guest bytes. State equality is on the amended plan's precisely enumerated
//! fields, compared at dispatcher re-entry after the side exit: EAX, the lazy pending-flag
//! descriptor (ADD's flag effects lowered into the descriptor stores per the base design's
//! rule), and EIP at instruction end. Compile latency around `finalize` is printed as an
//! order-of-magnitude smoke check only.

use super::super::*;

use crate::jit::clif::ClifBackend;
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, ExternalName, Function, InstBuilder, MemFlagsData, Signature,
    UserExternalName, UserFuncName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

const ENTRY: u32 = 0x500;
const SIDE_EXIT_DISPOSITION: i64 = 0xDEAD;
/// `add eax, ebx` in 32-bit code: 01 D8, two bytes.
const GUEST_BYTES: [u8; 2] = [0x01, 0xd8];

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.set_native_backend_enabled(false);
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

/// Build the tail-convention unit: load EAX/EBX through `offset_of!` on the real `CpuGsw`
/// fields (never hardcoded offsets), add, store EAX back, lower ADD's flag effects into the
/// interpreter's lazy pending-flag descriptor, advance EIP past the instruction, and return
/// the side-exit disposition through the adapter.
fn build_add_unit() -> Function {
    let gpr_base = core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, gpr);
    let eax_off = i32::try_from(gpr_base).expect("eax offset fits");
    let ebx_off = i32::try_from(gpr_base + 3 * 4).expect("ebx offset fits");
    let eip_off = i32::try_from(
        core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip),
    )
    .expect("eip offset fits");
    let pf_off = i32::try_from(core::mem::offset_of!(CpuGsw, pending_flags))
        .expect("pending flags offset fits");

    let mut sig = Signature::new(CallConv::Tail);
    sig.params.push(AbiParam::new(types::I64)); // &mut CpuGsw
    sig.returns.push(AbiParam::new(types::I64)); // disposition
    let mut func = Function::with_name_signature(UserFuncName::user(0, 10), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let flags = MemFlagsData::trusted();

    let a = builder.ins().load(types::I32, flags, cpu, eax_off);
    let b = builder.ins().load(types::I32, flags, cpu, ebx_off);
    let result = builder.ins().iadd(a, b);
    builder.ins().store(flags, result, cpu, eax_off);

    // The lazy pending-flag descriptor for a dword ADD, exactly as the interpreter's
    // `alu_add`/`jit_set_pending_add` write it: tag = present | dword width | add op,
    // then a (the destination's prior value), b (the source), and the result.
    let tag = builder.ins().iconst(types::I32, 0x8000_0200);
    builder.ins().store(flags, tag, cpu, pf_off);
    builder.ins().store(flags, a, cpu, pf_off + 4);
    builder.ins().store(flags, b, cpu, pf_off + 8);
    builder.ins().store(flags, result, cpu, pf_off + 12);

    let eip = builder.ins().load(types::I32, flags, cpu, eip_off);
    let eip = builder
        .ins()
        .iadd_imm(eip, i64::from(GUEST_BYTES.len() as u32));
    builder.ins().store(flags, eip, cpu, eip_off);

    let disposition = builder.ins().iconst(types::I64, SIDE_EXIT_DISPOSITION);
    builder.ins().return_(&[disposition]);
    builder.finalize();
    func
}

/// The dispatcher-shaped adapter for this unit: host default convention, one ordinary
/// `call_indirect` into the tail convention, forwarding the unit's disposition.
fn build_unit_adapter(backend: &mut ClifBackend) -> *const u8 {
    let mut tail_sig = Signature::new(CallConv::Tail);
    tail_sig.params.push(AbiParam::new(types::I64));
    tail_sig.returns.push(AbiParam::new(types::I64));
    let mut sig = Signature::new(backend.isa().default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // &mut CpuGsw
    sig.params.push(AbiParam::new(types::I64)); // unit entry
    sig.returns.push(AbiParam::new(types::I64)); // disposition
    let mut func = Function::with_name_signature(UserFuncName::user(0, 11), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(tail_sig);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let callee = builder.block_params(entry)[1];
    let call = builder.ins().call_indirect(sig_ref, callee, &[cpu]);
    let disposition = builder.inst_results(call)[0];
    builder.ins().return_(&[disposition]);
    builder.finalize();
    backend
        .finalize(func)
        .expect("adapter compiles with zero relocations")
}

#[test]
fn clif_register_unit_matches_the_interpreter_state() {
    const EAX_IN: u32 = 0x7fff_fff0;
    const EBX_IN: u32 = 0x0000_0021;

    // Interpreter reference: the equivalent guest bytes retired through the real decode and
    // execute path in a flat 32-bit code segment.
    let mut memory = vec![0xf4u8; 0x1000];
    memory[ENTRY as usize..ENTRY as usize + GUEST_BYTES.len()].copy_from_slice(&GUEST_BYTES);
    let mut interpreter = flat_cpu();
    interpreter.registers.set_eax(EAX_IN);
    interpreter.registers.set_ebx(EBX_IN);
    let mut bus = TestBus::with_memory(memory);
    interpreter
        .cycle(&mut bus)
        .expect("interpreter retires add eax, ebx");

    // Native unit: compile (latency printed as an order-of-magnitude smoke check), install,
    // and execute against a real CpuGsw through the dispatcher-shaped adapter.
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let unit_func = build_add_unit();
    let started = std::time::Instant::now();
    let unit = backend
        .finalize(unit_func)
        .expect("unit compiles with zero relocations");
    let compile_ns = started.elapsed().as_nanos();
    println!("clif compile_ns={compile_ns}");
    let adapter_ptr = build_unit_adapter(&mut backend);
    // SAFETY: built at the host default convention with exactly this signature; spans are
    // sealed executable for the backend's lifetime.
    let adapter: extern "C" fn(*mut CpuGsw, *const u8) -> i64 =
        unsafe { std::mem::transmute(adapter_ptr) };

    let mut native = flat_cpu();
    native.registers.set_eax(EAX_IN);
    native.registers.set_ebx(EBX_IN);
    let disposition = adapter(&mut native, unit);

    // Dispatcher re-entry after the side exit: the enumerated equality set.
    assert_eq!(disposition, SIDE_EXIT_DISPOSITION);
    assert_eq!(native.registers.eax(), interpreter.registers.eax());
    assert_eq!(native.registers.eax(), EAX_IN.wrapping_add(EBX_IN));
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "the lowered descriptor stores must equal the interpreter's lazy descriptor"
    );
    assert_eq!(native.eflags(), interpreter.eflags());
    assert_eq!(native.registers.eip, interpreter.registers.eip);
    assert_eq!(native.registers.eip, ENTRY + GUEST_BYTES.len() as u32);
}

/// A1 (dev_docs/plans/2026-07-19-clif-compile-second-cause-design.md section 3.7): the
/// sticky arena-exhausted flag must set ONLY when a finalized unit fails to install for lack
/// of remaining arena capacity, never for a codegen error or the zero-relocation install
/// invariant's own reject (adversarial review MINOR-5 -- those are per-unit failures, not
/// evidence the arena itself is full). A codegen error returns via `finalize`'s very first
/// `ctx.compile(..).ok()?`, before the capacity check ever runs, so only the relocation
/// reject needs a runtime proof here; the codegen-error case is structurally excluded by the
/// early return alone.
#[test]
fn clif_arena_exhausted_sets_only_on_capacity_not_on_relocation_reject() {
    // A backend whose arena holds exactly one rounded-up unit span (the `with_len_for_test`
    // seam, `exec_mem.rs`): the first `finalize` fills it completely, so a second unit's
    // `finalize` fails purely for lack of capacity, never for any other reason.
    let page = crate::jit::exec_mem::host_page_len();
    let mut backend =
        ClifBackend::with_arena_len_for_test(page).expect("small test arena on a supported host");
    assert!(
        !backend.arena_exhausted(),
        "a fresh backend starts unexhausted"
    );

    let first = backend
        .finalize(build_add_unit())
        .expect("the first unit fits the one-page arena exactly");
    assert!(!first.is_null());
    assert!(
        !backend.arena_exhausted(),
        "installing the first unit must not itself set the flag"
    );

    // A second, otherwise-identical unit: compiles fine, but the one-page arena has no room
    // left at all (the first install rounded up to and consumed the whole page).
    assert!(
        backend.finalize(build_add_unit()).is_none(),
        "the second unit must fail: the one-page arena is already full"
    );
    assert!(
        backend.arena_exhausted(),
        "a capacity failure must set the sticky flag"
    );

    // A FRESH backend, ample room, but a function whose body makes a genuine external call
    // (not the `call_indirect`-through-a-baked-constant shape the real lowering always uses):
    // the compiled buffer carries a real relocation, so the zero-relocation install invariant
    // rejects it -- a per-unit failure, NOT arena exhaustion.
    let mut fresh = ClifBackend::new().expect("pinned host ISA on a supported host");
    assert_eq!(fresh.relocation_fallbacks(), 0);
    let external_call_fn = build_function_with_external_call(&fresh);
    assert!(
        fresh.finalize(external_call_fn).is_none(),
        "a function with an unresolved external call must be rejected for its relocation"
    );
    assert_eq!(
        fresh.relocation_fallbacks(),
        1,
        "the reject must be attributed to the relocation invariant"
    );
    assert!(
        !fresh.arena_exhausted(),
        "a relocation reject must NEVER set the arena-exhausted flag (MINOR-5)"
    );

    // The ample-room backend still admits a real, relocation-free unit normally afterward --
    // proof the relocation reject didn't wrongly poison later admissions either.
    assert!(fresh.finalize(build_add_unit()).is_some());
    assert!(!fresh.arena_exhausted());
}

/// Track C A2 (`dev_docs/plans/2026-07-19-clif-arena-reset-design.md` sections 7-9): the
/// backend-level half of the durability proof. A one-page arena fills exactly like the A1
/// test above, `arena_exhausted()` latches, and the cached adapter/sentinel handles are built.
/// `reset_arena` must reclaim the arena (capacity comes back), clear `arena_exhausted` (the A1
/// interaction, design section 9), and drop both cached handles so they rebuild lazily
/// (design section 8) -- proven here by compiling a fresh unit AND rebuilding the adapter and
/// sentinel afterward, all against the SAME backend instance.
#[test]
fn clif_reset_arena_reclaims_capacity_and_invalidates_cached_handles() {
    // Three pages: room for the adapter (1), the sentinel trampoline (1), and exactly one
    // `build_add_unit()` (1) before the arena is genuinely full.
    let page = crate::jit::exec_mem::host_page_len();
    let mut backend = ClifBackend::with_arena_len_for_test(3 * page)
        .expect("small test arena on a supported host");

    // Build both arena-resident handles BEFORE filling the rest of the arena, so their
    // pre-reset presence is unambiguous (a fresh backend's `is_none()` on both fields is the
    // trivial, uninteresting case; this proves the reset actively invalidates handles that
    // were actually populated).
    assert!(backend.callout_adapter().is_some());
    assert!(backend.sentinel_descriptor().is_some());
    assert!(
        !backend.callout_adapter_and_sentinel_are_unset_for_test(),
        "both handles must be populated before the reset under test"
    );

    // One more unit exactly fills the remaining page; the next one fails.
    assert!(backend.finalize(build_add_unit()).is_some());
    assert!(!backend.arena_exhausted());
    assert!(backend.finalize(build_add_unit()).is_none());
    assert!(
        backend.arena_exhausted(),
        "a fourth unit must fail: only three pages exist"
    );
    assert_eq!(
        backend.arena_used_slots_for_test(),
        3,
        "adapter+sentinel+one unit"
    );

    assert!(
        backend.reset_arena(0),
        "reset must succeed on a supported host"
    );
    assert!(!backend.arena_exhausted(), "A1's flag must clear on reset");
    assert_eq!(
        backend.arena_used_slots_for_test(),
        0,
        "the arena must be empty again"
    );
    assert!(
        backend.callout_adapter_and_sentinel_are_unset_for_test(),
        "both cached handles must be invalidated so they rebuild against the fresh arena"
    );

    // The durability proof: a unit that would have failed a moment ago (the arena was full)
    // now installs, and the two handles rebuild lazily on next use.
    assert!(
        backend.finalize(build_add_unit()).is_some(),
        "a previously-failing unit must install after the reset"
    );
    assert!(!backend.arena_exhausted());
    assert!(backend.callout_adapter().is_some(), "adapter must rebuild");
    assert!(
        backend.sentinel_descriptor().is_some(),
        "sentinel must rebuild"
    );
}

/// Track C A2 (design section 6, MAJOR-1/MINOR-4): the release-safe guard. With a (today
/// impossible; design section 5) live native frame simulated by forcing `native_frame_depth`
/// nonzero, `apply_deferred_clif_arena_reset` must SKIP the reset and leave
/// `backend_needs_reset` SET -- proven here by observing the backend's cached handles survive
/// untouched despite a pending reset request. In a debug build the guard additionally trips a
/// loud `debug_assert!`; this test catches that panic (exactly as the x87 shim's own belt
/// does in production) so the SAME test can then check the release-relevant invariant: the
/// branch that runs before any panic, and unconditionally in a release build where the assert
/// compiles to nothing, must leave every bit of state untouched. A second call with the depth
/// reported back to zero then proves the pending reset was never lost -- it reclaims on the
/// very next frame-free attempt.
#[test]
fn clif_deferred_reset_guard_skips_and_preserves_state_when_a_frame_is_reported_live() {
    let page = crate::jit::exec_mem::host_page_len();
    let mut cpu = flat_cpu();
    cpu.set_clif_backend_enabled(true);
    cpu.jit_direct.clif_backend = crate::jit::clif::ClifBackend::with_arena_len_for_test(4 * page);
    {
        let backend = cpu
            .jit_direct
            .clif_backend
            .as_mut()
            .expect("small test backend");
        assert!(backend.callout_adapter().is_some());
        assert!(backend.sentinel_descriptor().is_some());
    }

    cpu.jit_direct.backend_needs_reset = true;
    cpu.jit_direct.native_frame_depth = 1;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cpu.jit_direct.apply_deferred_clif_arena_reset();
    }));
    if cfg!(debug_assertions) {
        assert!(
            result.is_err(),
            "a live-frame reset attempt must trip the debug assert"
        );
    } else {
        assert!(result.is_ok(), "a release build must not panic here");
    }

    // The release-relevant invariant, true regardless of whether the debug assert fired: the
    // flag stays set (deferred, never silently dropped) and the backend's arena-resident
    // handles were never touched (proof `reset_arena` did not run under the live frame).
    assert!(
        cpu.jit_direct.backend_needs_reset,
        "a live-frame skip must leave the reset pending"
    );
    assert!(
        !cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend still present")
            .callout_adapter_and_sentinel_are_unset_for_test(),
        "a skipped reset must not touch the backend's cached handles"
    );

    // Report the frame gone (depth == 0, the normal post-unwind state): the SAME pending flag
    // reclaims on the very next call, with nothing left to block it.
    cpu.jit_direct.native_frame_depth = 0;
    cpu.jit_direct.apply_deferred_clif_arena_reset();
    assert!(
        !cpu.jit_direct.backend_needs_reset,
        "a frame-free retry must consume the deferred reset"
    );
    assert!(
        cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend still present")
            .callout_adapter_and_sentinel_are_unset_for_test(),
        "the deferred reset must invalidate the cached handles once it actually runs"
    );
}

/// The drop guard itself (design section 6, MINOR-4): `NativeFrameGuard` must decrement
/// `native_frame_depth` on an unwind, not only on a normal return, so a caught call-out panic
/// (`run_clif_unit`'s `resume_unwind` path) can never wedge the depth permanently nonzero and
/// permanently suppress every future reset.
#[test]
fn clif_native_frame_guard_decrements_on_unwind() {
    let mut cpu = flat_cpu();
    cpu.set_clif_backend_enabled(true);
    assert_eq!(cpu.jit_direct.native_frame_depth, 0);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _frame = unsafe {
            crate::jit::NativeFrameGuard::enter(std::ptr::from_mut(
                &mut cpu.jit_direct.native_frame_depth,
            ))
        };
        assert_eq!(cpu.jit_direct.native_frame_depth, 1, "enter must increment");
        panic!("simulate a call-out panic crossing the guard's scope");
    }));
    assert!(result.is_err());
    assert_eq!(
        cpu.jit_direct.native_frame_depth, 0,
        "the guard must decrement on the unwind path, not only on a normal return"
    );
}

/// Build a minimal function whose body makes a genuine external `call` -- referencing an
/// unresolved user external symbol rather than the `call_indirect`-through-a-baked-pointer
/// shape the real unit/adapter lowering always uses -- so the compiled buffer carries a real
/// relocation the linker would need to patch: the zero-relocation install invariant's OTHER
/// rejection reason, distinct from arena exhaustion.
fn build_function_with_external_call(backend: &ClifBackend) -> Function {
    let call_conv = backend.isa().default_call_conv();
    let mut sig = Signature::new(call_conv);
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = Function::with_name_signature(UserFuncName::user(0, 30), sig.clone());
    let callee_name = func.declare_imported_user_function(UserExternalName::new(0, 999));
    let callee_sig_ref = func.import_signature(sig);
    let callee = func.import_function(ExtFuncData {
        name: ExternalName::user(callee_name),
        signature: callee_sig_ref,
        colocated: false,
        patchable: false,
    });
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let call = builder.ins().call(callee, &[]);
    let result = builder.inst_results(call)[0];
    builder.ins().return_(&[result]);
    builder.finalize();
    func
}

use crate::jit::clif::cache::{ClifUnitState, clif_key_for, walk_unit};

fn warm_lines(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &offset in starts {
        cpu.set_eip(offset);
        cpu.fetch_decoded(bus, offset).expect("warm decode");
    }
    cpu.set_eip(ENTRY);
}

/// C1a growth walker (F-A5): terminates on a Jcc terminal (included), computes the guest
/// byte layout, flags the self-loop, and stops at the first unclassifiable opcode.
#[test]
fn clif_walker_terminates_on_jcc_and_flags_the_self_loop() {
    // inc eax; add eax, ebx; jnz -4 (back to entry): 1 + 2 + 2 bytes.
    let code = [0x40, 0x01, 0xd8, 0x75, 0xfb];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 3]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 3);
    assert_eq!(layout.guest_len, 5);
    assert_eq!(&layout.fetch_lens[..3], &[1, 2, 2]);
    assert!(layout.is_self_loop, "taken target is the unit entry");
    assert!(!layout.has_wide_accesses, "register-only unit");

    // The same body with a forward Jcc is not a self-loop.
    let code = [0x40, 0x01, 0xd8, 0x75, 0x02];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 3]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 3);
    assert!(!layout.is_self_loop);
}

/// Q1 stop-growth: the first structurally unclassifiable opcode ends the unit BEFORE it,
/// and a cold line ends it the same way.
#[test]
fn clif_walker_stops_before_an_unclassifiable_opcode() {
    // inc eax; hlt (0xf4 is not classifiable by the Direct classifier).
    let code = [0x40, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 1);
    assert_eq!(layout.guest_len, 1);
    // An entry that is itself unclassifiable yields no unit at all.
    assert!(walk_unit(&cpu, ENTRY + 1, true).is_none());
}

/// K1-K5: the clif key applies the same static exclusions as direct::key_for.
#[test]
fn clif_key_for_applies_the_direct_static_exclusions() {
    let code = [0x40, 0x75, 0xfd];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY]);
    let key = clif_key_for(&cpu, ENTRY, true).expect("flat 586 key");
    assert_eq!(key.linear, ENTRY);
    assert_eq!(key.physical, ENTRY);
    assert_eq!(key.mode_key, cpu.jit_mode_key());
    // K2: the 16-bit decode variant never keys.
    assert!(clif_key_for(&cpu, ENTRY, false).is_none());
    // K3: only the Approximate personas key.
    cpu.set_mode(GswMode::Gsw386);
    assert!(clif_key_for(&cpu, ENTRY, true).is_none());
    cpu.set_mode(GswMode::Gsw586);
    // K4: the BIOS F-page window never keys.
    assert!(clif_key_for(&cpu, 0x000f_f000, true).is_none());
    assert!(clif_key_for(&cpu, 0x000f_f3ff, true).is_none());
    // K5: a cold line (no physical) never keys.
    assert!(clif_key_for(&cpu, ENTRY + 0x100, true).is_none());
}

/// Cache admission states mirror the Direct roles: Seen before install, Compiled after,
/// Dormant parks, and install refuses keys not in Seen.
#[test]
fn clif_unit_cache_tracks_seen_compiled_and_dormant() {
    use crate::jit::clif::cache::{ClifUnitCache, ClifUnitDescriptor, ClifUnitKey};
    use crate::jit::code_watch::NativeCodeWatch;
    use crate::jit::direct::{MAX_BLOCK_INSTRUCTIONS, SegmentLayout};
    let mut cache = ClifUnitCache::default();
    let mut watch = NativeCodeWatch::default();
    let key = ClifUnitKey {
        linear: 0x1000,
        physical: 0x1000,
        mode_key: 7,
    };
    let descriptor = ClifUnitDescriptor {
        key,
        guest_len: 3,
        fetch_lens: [0; MAX_BLOCK_INSTRUCTIONS],
        instructions: 2,
        segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0).expect("default layout"),
        memory_cpl3: false,
        has_wide_accesses: false,
        is_self_loop: false,
        entry: 0,
        operands: [0; 2 * MAX_BLOCK_INSTRUCTIONS],
        leading: 1,
        x87_mask: 0,
        cum_raw_before: [0; MAX_BLOCK_INSTRUCTIONS],
        cum_lowered_before: [0; MAX_BLOCK_INSTRUCTIONS],
        raw_clocks_total: 2,
        lowered_total: 1,
        cum_access_before: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        access_total: Default::default(),
        terminal: false,
        disp_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_extend: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        lea_mask: 0,
        moffs_mask: 0,
        interp_once: false,
        code_host: 0,
        successors: [None; 2],
    };
    // A stand-in sentinel-descriptor address (any stable nonzero address works for a
    // linkless unit test); fresh cells are sentinel-repointed per the N1a discipline.
    let sentinel_marker = 0u64;
    let sentinel_addr = std::ptr::from_ref(&sentinel_marker) as usize;
    let sentinel_portal = cache.sentinel_portal(sentinel_addr);
    let make_cells = || {
        let cells = [
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
        ];
        for cell in &cells {
            cell.set(sentinel_portal.as_ref());
        }
        cells
    };
    assert!(cache.state(key).is_none());
    // Install without Seen refuses.
    assert!(
        cache
            .install(&mut watch, descriptor.clone(), make_cells(), sentinel_addr)
            .is_none()
    );
    cache.note_seen(key);
    assert_eq!(cache.state(key), Some(ClifUnitState::Seen));
    let index = cache
        .install(&mut watch, descriptor, make_cells(), sentinel_addr)
        .expect("install after Seen");
    assert_eq!(cache.state(key), Some(ClifUnitState::Compiled(index)));
    assert_eq!(cache.unit(index).expect("descriptor").instructions, 2);
    // M5: the installed unit's own guest physical range reads watched immediately, proving
    // the install-time `acquire_range` registration actually happened (design section 5
    // test 3's registration probe), not merely that the check machinery works when a watch
    // happens to exist for some other reason.
    assert!(watch.range_watched(key.physical, 3));
    // A separate Seen key parks Dormant without ever acquiring a registration.
    let dormant_key = ClifUnitKey {
        linear: 0x2000,
        physical: 0x2000,
        mode_key: 7,
    };
    cache.note_seen(dormant_key);
    cache.park_dormant(dormant_key);
    assert_eq!(cache.state(dormant_key), Some(ClifUnitState::Dormant));
    assert!(!watch.range_watched(dormant_key.physical, 3));
    cache.clear(&mut watch);
    assert!(cache.state(key).is_none());
    assert!(cache.state(dormant_key).is_none());
    // Clearing the cache releases the registration too: the shared watch reports the range
    // unwatched again once the unit is gone (no leaked refcount past a wholesale drop).
    assert!(!watch.range_watched(key.physical, 3));
}

/// C1b probe: one tiny lowered unit end to end against the interpreter.
#[test]
fn clif_lowered_probe_minimal() {
    // inc eax; add eax,ebx; jnz +1 (forward, not taken when zf...) then hlt
    let code = [0x40, 0x01, 0xd8, 0x75, 0x01, 0xf4, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut interp = flat_cpu();
    interp.registers.set_eax(5);
    interp.registers.set_ebx(7);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    for _ in 0..8 {
        interp.set_eip(ENTRY);
        interp.halted = false;
        loop {
            let outcome = interp
                .run_budgeted(&mut interp_bus, 4096)
                .expect("interp run");
            if outcome.halted {
                break;
            }
        }
    }

    let mut clif = flat_cpu();
    clif.set_clif_backend_enabled(true);
    clif.registers.set_eax(5);
    clif.registers.set_ebx(7);
    let mut clif_bus = TestBus::with_memory(memory.clone());
    clif_bus.direct_pages_enabled = true;
    clif_bus.direct_page_clocks = true;
    for pass in 0..8 {
        clif.set_eip(ENTRY);
        clif.halted = false;
        loop {
            let outcome = clif.run_budgeted(&mut clif_bus, 4096).expect("clif run");
            if outcome.halted {
                break;
            }
        }
        println!(
            "pass {pass}: eip={:#x} eax={:#x} eflags_raw={:#x} pending={:x?} entries={}",
            clif.registers.eip,
            clif.registers.eax(),
            clif.registers.eflags,
            clif.pending_flags,
            clif.jit_clif_counters().entries
        );
    }

    assert!(
        clif.jit_clif_counters().entries > 0,
        "unit never entered: {:#?}",
        clif.jit_clif_counters()
    );
    assert_eq!(clif.registers, interp.registers);
    assert_eq!(clif.eflags(), interp.eflags());
    assert_eq!(clif.pending_flags, interp.pending_flags);
    assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        clif_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
}

/// C1b probe: straight-line unit with no terminal, full-state check per pass.
#[test]
fn clif_lowered_probe_straight_line() {
    // inc eax; add eax,ebx; hlt
    let code = [0x40, 0x01, 0xd8, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut interp = flat_cpu();
    interp.registers.set_eax(5);
    interp.registers.set_ebx(7);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;

    let mut clif = flat_cpu();
    clif.set_clif_backend_enabled(true);
    clif.registers.set_eax(5);
    clif.registers.set_ebx(7);
    let mut clif_bus = TestBus::with_memory(memory.clone());
    clif_bus.direct_pages_enabled = true;
    clif_bus.direct_page_clocks = true;

    for pass in 0..6 {
        for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
            cpu.set_eip(ENTRY);
            cpu.halted = false;
            loop {
                let outcome = cpu.run_budgeted(bus, 4096).expect("run");
                if outcome.halted {
                    break;
                }
            }
        }
        assert_eq!(clif.registers, interp.registers, "pass {pass}");
        assert_eq!(clif.pending_flags, interp.pending_flags, "pass {pass}");
        assert_eq!(
            clif.registers.eflags, interp.registers.eflags,
            "pass {pass}"
        );
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "pass {pass}");
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "pass {pass}"
        );
    }
    assert!(clif.jit_clif_counters().entries > 0);
}

/// C1f (`dev_docs/plans/2026-07-19-clif-compile-churn-fix-design.md`, Option 2), the
/// LEAD/load-bearing regression: a write elsewhere on a shared 4KB physical page must not
/// evict `Seen`/`Dormant` entries anymore. Both states are bare markers (`cache.rs:602-607`
/// carries no span/guest_len/watch registration for either), so there is nothing a stale
/// write could invalidate; the eventual promotion attempt always re-walks LIVE bytes via
/// `walk_unit` regardless of what happened to the bookkeeping meanwhile. Before the fix this
/// test FAILS the predicted way: both same-page entries are dropped (`state` -> `None`) and
/// `kills_no_layout == 2`. A companion `Compiled` unit on a DIFFERENT page proves the
/// byte-exact `Compiled` arm (real SMC handling) is completely untouched by the fix, both by
/// surviving an unrelated write and by still dying on a write that actually overlaps it.
#[test]
fn clif_invalidate_physical_range_no_longer_evicts_seen_dormant_on_page_overlap() {
    use crate::jit::clif::cache::{ClifUnitCache, ClifUnitDescriptor, ClifUnitKey};
    use crate::jit::code_watch::NativeCodeWatch;
    use crate::jit::direct::{MAX_BLOCK_INSTRUCTIONS, SegmentLayout};

    let mut cache = ClifUnitCache::default();
    let mut watch = NativeCodeWatch::default();

    // Two keys sharing ONE 4KB physical page (0x2000..0x2fff): Seen at 0x2000, Dormant at
    // 0x2040. Neither has a span; the buggy arm dropped both on ANY write anywhere in the
    // shared page, regardless of how far it lands from either key's own address.
    let seen_key = ClifUnitKey {
        linear: 0x2000,
        physical: 0x2000,
        mode_key: 7,
    };
    let dormant_key = ClifUnitKey {
        linear: 0x2040,
        physical: 0x2040,
        mode_key: 7,
    };
    cache.note_seen(seen_key);
    cache.note_seen(dormant_key);
    cache.park_dormant(dormant_key);
    assert_eq!(cache.state(seen_key), Some(ClifUnitState::Seen));
    assert_eq!(cache.state(dormant_key), Some(ClifUnitState::Dormant));

    // A real Compiled unit on a DIFFERENT page (0x5000), single 3-byte slot with an empty
    // tail (disp_len/imm_len both 0), so any write overlapping its 3 bytes is unambiguously
    // structural (Kill), independent of the fix under test.
    let compiled_key = ClifUnitKey {
        linear: 0x5000,
        physical: 0x5000,
        mode_key: 7,
    };
    let mut fetch_lens = [0u8; MAX_BLOCK_INSTRUCTIONS];
    fetch_lens[0] = 3;
    let descriptor = ClifUnitDescriptor {
        key: compiled_key,
        guest_len: 3,
        fetch_lens,
        instructions: 1,
        segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0).expect("default layout"),
        memory_cpl3: false,
        has_wide_accesses: false,
        is_self_loop: false,
        entry: 0,
        operands: [0; 2 * MAX_BLOCK_INSTRUCTIONS],
        leading: 1,
        x87_mask: 0,
        cum_raw_before: [0; MAX_BLOCK_INSTRUCTIONS],
        cum_lowered_before: [0; MAX_BLOCK_INSTRUCTIONS],
        raw_clocks_total: 3,
        lowered_total: 1,
        cum_access_before: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        access_total: Default::default(),
        terminal: false,
        disp_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_extend: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        lea_mask: 0,
        moffs_mask: 0,
        interp_once: false,
        code_host: 0,
        successors: [None; 2],
    };
    let sentinel_marker = 0u64;
    let sentinel_addr = std::ptr::from_ref(&sentinel_marker) as usize;
    let sentinel_portal = cache.sentinel_portal(sentinel_addr);
    let make_cells = || {
        let cells = [
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
        ];
        for cell in &cells {
            cell.set(sentinel_portal.as_ref());
        }
        cells
    };
    cache.note_seen(compiled_key);
    cache
        .install(&mut watch, descriptor, make_cells(), sentinel_addr)
        .expect("install after Seen");
    assert!(matches!(
        cache.state(compiled_key),
        Some(ClifUnitState::Compiled(_))
    ));

    // The write under test: one byte at 0x2020. On the SAME page as both seen_key
    // (0x2000) and dormant_key (0x2040), but touching neither key's own address, and
    // nowhere near the Compiled unit's page (0x5000) at all.
    let outcome = cache.invalidate_physical_range(&mut watch, 0x2020, 1);

    // THE FIX under test: page-sharing alone must no longer evict Seen/Dormant
    // bookkeeping. (Pre-fix, this is exactly where the test fails: both states become
    // `None` and `outcome.kills_no_layout == 2`.)
    assert_eq!(
        cache.state(seen_key),
        Some(ClifUnitState::Seen),
        "a Seen entry must survive an unrelated same-page write"
    );
    assert_eq!(
        cache.state(dormant_key),
        Some(ClifUnitState::Dormant),
        "a Dormant entry must survive an unrelated same-page write"
    );
    assert_eq!(outcome.kills_no_layout, 0, "{outcome:?}");
    // Unaffected either way: a different page entirely, and the Compiled arm was never in
    // scope for this write.
    assert!(matches!(
        cache.state(compiled_key),
        Some(ClifUnitState::Compiled(_))
    ));

    // Companion regression guard: the Compiled arm's real, byte-exact SMC handling is
    // completely untouched by the fix. A write landing INSIDE the compiled unit's own span
    // (0x5000..0x5003) must still kill it, exactly as before.
    let outcome2 = cache.invalidate_physical_range(&mut watch, 0x5000, 1);
    assert_eq!(
        cache.state(compiled_key),
        None,
        "a write that actually overlaps a Compiled unit's span must still kill it"
    );
    assert_eq!(outcome2.kills, 1, "{outcome2:?}");
}

/// Track C A2 (`dev_docs/plans/2026-07-19-clif-arena-reset-design.md`): the production-path
/// durability proof (design section 11, test 1). A tiny 3-page backend (room for exactly one
/// unit's sentinel trampoline + shared adapter + body, design section 8) admits unit A and
/// runs it natively, filling the arena; a different-address unit B then fails to install --
/// the arena is genuinely full and A1's `arena_exhausted` flag latches, exactly the pre-A2
/// dead end (`dev_docs/plans/2026-07-19-clif-compile-second-cause-design.md` section 3.7).
/// A wholesale `clif_clear()` (standing in for the paging/mode/SMC invalidation events that
/// reach it in production, `core.rs:117/150/389`) DEFERS the reset -- the arena stays full and
/// `arena_exhausted()` stays latched until the next admission. Driving one more admission
/// through the real production path (`run_budgeted`, never a direct call into the reset
/// primitive) reclaims the arena at the top of `try_clif_continuation`, and unit B -- the
/// previously-failing key -- now installs and runs native, matching the interpreter exactly:
/// this is "A2 makes clif a JIT past the first arena fill."
#[test]
fn clif_arena_reset_reclaims_after_a_wholesale_clear_and_reinstalls() {
    const ENTRY_A: u32 = 0x1000;
    const ENTRY_B: u32 = 0x2000;
    // The batch loop's very first retired instruction after `set_eip` is always interpreted
    // (`run.rs`'s `first` step), so the clif-admitted unit actually begins at ENTRY+1: a
    // second, real `inc` instruction there, followed by `hlt`, gives the walker a one-slot
    // leading run to lower (mirrors `clif_lowered_probe_straight_line`'s shape).
    let code_a = [0x40, 0x40, 0xf4]; // inc eax; inc eax; hlt
    let code_b = [0x43, 0x43, 0xf4]; // inc ebx; inc ebx; hlt

    let mut memory = vec![0xf4u8; 0x3000];
    memory[ENTRY_A as usize..ENTRY_A as usize + code_a.len()].copy_from_slice(&code_a);
    memory[ENTRY_B as usize..ENTRY_B as usize + code_b.len()].copy_from_slice(&code_b);

    let mut cpu = flat_cpu();
    cpu.set_clif_backend_enabled(true);
    let page = crate::jit::exec_mem::host_page_len();
    cpu.jit_direct.clif_backend = ClifBackend::with_arena_len_for_test(3 * page);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;

    fn run_to_halt_from(cpu: &mut CpuGsw, bus: &mut TestBus, entry: u32) {
        cpu.set_eip(entry);
        cpu.halted = false;
        loop {
            let outcome = cpu.run_budgeted(bus, 4096).expect("runs");
            if outcome.halted {
                break;
            }
        }
    }

    // Unit A: warm it up until it compiles and runs native, consuming the whole 3-page arena
    // (the sentinel trampoline, the shared adapter, and A's own body -- design section 8).
    for _ in 0..6 {
        run_to_halt_from(&mut cpu, &mut bus, ENTRY_A);
    }
    let key_a = clif_key_for(&cpu, ENTRY_A + 1, true).expect("flat 586 key");
    assert!(
        matches!(
            cpu.jit_direct.clif_units.state(key_a),
            Some(ClifUnitState::Compiled(_))
        ),
        "unit A must compile and install"
    );
    assert_eq!(cpu.jit_clif_counters().units_installed, 1);
    assert!(
        !cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend present")
            .arena_exhausted()
    );

    // Unit B: a DIFFERENT address. The arena has no room left, so its compile fails and the
    // sticky A1 flag latches.
    for _ in 0..6 {
        run_to_halt_from(&mut cpu, &mut bus, ENTRY_B);
    }
    let key_b = clif_key_for(&cpu, ENTRY_B + 1, true).expect("flat 586 key");
    assert!(
        !matches!(
            cpu.jit_direct.clif_units.state(key_b),
            Some(ClifUnitState::Compiled(_))
        ),
        "unit B must fail to install: the arena is full"
    );
    assert!(
        cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend present")
            .arena_exhausted(),
        "a genuine capacity failure must latch A1's flag"
    );
    assert_eq!(
        cpu.jit_clif_counters().units_installed,
        1,
        "B must not install"
    );

    // The wholesale clear: cache torn down immediately, arena reset DEFERRED (design
    // section 3) -- the load-bearing deferral proof.
    cpu.jit_direct.clif_clear();
    assert!(
        cpu.jit_direct.backend_needs_reset,
        "the clear must request a reset"
    );
    assert_eq!(
        cpu.jit_direct.clif_units.state(key_a),
        None,
        "the clear drops every cached admission state"
    );
    assert!(
        cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend present")
            .arena_exhausted(),
        "deferral: the arena must NOT be reset yet"
    );
    assert_eq!(
        cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend present")
            .arena_used_slots_for_test(),
        3,
        "deferral: the arena stays full until the next frame-free admission"
    );

    // Drive one more admission through the REAL production path: the very first
    // `try_clif_continuation` call reclaims the arena at its top, before anything else runs.
    for _ in 0..6 {
        run_to_halt_from(&mut cpu, &mut bus, ENTRY_B);
    }
    assert!(
        !cpu.jit_direct.backend_needs_reset,
        "the deferred reset must have consumed the flag by now"
    );
    assert!(
        !cpu.jit_direct
            .clif_backend
            .as_ref()
            .expect("backend present")
            .arena_exhausted(),
        "A1's flag must clear along with the reset"
    );
    assert!(
        matches!(
            cpu.jit_direct.clif_units.state(key_b),
            Some(ClifUnitState::Compiled(_))
        ),
        "the previously-failing key must install after the deferred reset"
    );
    assert_eq!(
        cpu.jit_clif_counters().units_installed,
        2,
        "a second unit must have installed after the reset (the durability proof)"
    );

    // State correctness (MINOR-6, design section 10): A2 changes WHICH executor retires the
    // instruction, never the resulting state. One more full pass, compared against a plain
    // interpreter (clif disabled) started from the same EBX, proves unit B's compiled body
    // (installed only after the deferred reset) is byte-identical, not merely "some code ran".
    let ebx_before = cpu.registers.ebx();
    let mut reference = flat_cpu();
    reference.registers.set_ebx(ebx_before);
    let mut reference_bus = TestBus::with_memory(vec![0xf4u8; 0x3000]);
    reference_bus.memory[ENTRY_B as usize..ENTRY_B as usize + code_b.len()]
        .copy_from_slice(&code_b);
    reference_bus.direct_pages_enabled = true;
    reference_bus.direct_page_clocks = true;
    run_to_halt_from(&mut reference, &mut reference_bus, ENTRY_B);

    run_to_halt_from(&mut cpu, &mut bus, ENTRY_B);
    // EAX is deliberately excluded: `cpu` also ran unit A repeatedly earlier in this same
    // test, accumulating unrelated `inc eax` retirements the reference (unit-B-only) CPU
    // never saw. EBX, the flags, and EIP are exactly what unit B's own body touches.
    assert_eq!(
        cpu.registers.ebx(),
        ebx_before + 2,
        "two inc ebx must retire"
    );
    assert_eq!(cpu.registers.ebx(), reference.registers.ebx());
    assert_eq!(cpu.registers.eip, reference.registers.eip);
    assert_eq!(cpu.pending_flags, reference.pending_flags);
    assert_eq!(cpu.eflags(), reference.eflags());
}

/// The walker bakes an UNMASKED taken target, twice: in the successor record and again in
/// `lower_terminal_jcc`'s spilled EIP. A Word-size relative branch masks its target to 16 bits,
/// and this walker has no equivalent of Direct's `control_target_limit` clamp, so growth stops
/// at a Word terminal instead.
///
/// The two `inc eax` fillers are load-bearing: `walk_unit` returns `None` at zero instructions,
/// so with the branch as the entry instruction there would be no layout to assert against.
///
/// The Dword control has to terminate on a **Jcc** specifically. A `Jmp` leaves `successors[1]`
/// unset either way, so it could not tell a Word-keyed stop from a Dword-keyed one.
#[test]
fn clif_walker_stops_before_a_word_size_control_transfer() {
    // inc eax; inc eax; 66 0f 85 10 00 (jnz +0x10 at Word operand size).
    let code = [0x40, 0x40, 0x66, 0x0f, 0x85, 0x10, 0x00];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 2, "the Word branch must not be walked");
    assert_eq!(layout.kinds.len(), 2);
    assert_eq!(layout.guest_len, 2);
    assert!(layout.successors[0].is_none());
    assert!(layout.successors[1].is_none());

    // Control: unprefixed, so Dword, and the walker takes it as a terminal with both edges.
    let code = [0x40, 0x40, 0x0f, 0x85, 0x10, 0x00, 0x00, 0x00];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 2]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 3);
    assert_eq!(layout.guest_len, 8);
    assert!(layout.successors[0].is_some(), "taken edge");
    assert!(layout.successors[1].is_some(), "fall-through edge");
}
