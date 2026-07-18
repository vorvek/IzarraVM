// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! C1b-pre proof battery: the widened five-parameter call-out ABI (design section 1.7),
//! proven standalone before any lowering builds on it, mirroring C0's tail-call proof
//! discipline. Every unit here uses the FINAL `ClifEntryFn` arity (cpu, bus_opaque, table,
//! imm_table, entry) with the four-live-parameter Tail unit signature (review finding M2), so
//! the battery covers exactly the ABI C1b-main's compiler emits, not a narrower stand-in. No
//! real `CpuGsw`/`CpuBus` is involved: the `cpu` parameter carries a `#[repr(C)]` proof
//! context and the shims are stubs, mirroring how `proof_test.rs` used synthetic bodies.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Function, InstBuilder, MemFlagsData, UserFuncName, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::ClifBackend;
use super::callout::{
    CLIF_CALLOUT_CONTINUE, CLIF_CALLOUT_EXIT, CLIF_CALLOUT_HARD_STOP, ClifCallOutTable,
    ClifEntryFn, callout_shim_signature, callout_unit_signature,
};
use crate::CpuGsw;

/// The proof context, passed through the ABI's `cpu` parameter (the stubs cast it back).
/// `#[repr(C)]` so the CLIF field offsets below are stable: hops 0x00, disposition 0x08,
/// rsp_first 0x10, rsp_last 0x18, remaining 0x20, unit_addr 0x28, resolver_addr 0x30,
/// eip 0x38, callouts 0x40, rsp_callout_mismatches 0x48, slots_executed 0x50,
/// pending_error 0x58, scratch 0x60, rsp_pre_callout 0x68, rsp_post_callout 0x70,
/// bad_dispositions 0x78.
#[repr(C)]
#[derive(Default)]
struct CallOutProofCtx {
    hops: u64,
    disposition: u64,
    rsp_first: u64,
    rsp_last: u64,
    remaining: u64,
    unit_addr: u64,
    resolver_addr: u64,
    /// The stub guest EIP: the Continue stub compares it against the baked
    /// `site_eip + fetch_len` fall-through (the B1 predicate shape); the fault stub mutates
    /// it to a synthetic handler address, modeling the interpreter's fault-delivery redirect.
    eip: u64,
    callouts: u64,
    /// Unit-side accumulation: how many call-outs returned with RSP different from its
    /// pre-call value (must stay zero; counted in CLIF so the 500k chain needs no per-hop
    /// Rust observation).
    rsp_callout_mismatches: u64,
    /// The two-slot unit bumps this in its post-call-out slot; a non-Continue disposition
    /// must exit before it (design battery item 3).
    slots_executed: u64,
    /// The hard-stop stub's stashed error value (stands in for jit_clif.pending_hard_error).
    pending_error: u64,
    scratch: u64,
    rsp_pre_callout: u64,
    rsp_post_callout: u64,
    /// Chain-unit accumulation: call-outs whose disposition was not Continue (must stay zero).
    bad_dispositions: u64,
}

const UNRESOLVED_DISPOSITION: i64 = 0xDEAD;
const SYNTHETIC_HANDLER_EIP: u64 = 0x0000_beef;
const STASHED_ERROR: u64 = 0xE44E;

/// Baked structural layout data for the proof call sites (the B1 predicate operands).
const SITE_EIP: u32 = 0x1234;
const FETCH_LEN: u32 = 2;

/// Continue stub: exercises the shim-side EIP-comparison shape of the B1 predicate. Continue
/// only when the context's stub EIP equals the baked fall-through for this site.
unsafe extern "C" fn continue_shim(
    cpu: *mut CpuGsw,
    _bus: *mut core::ffi::c_void,
    site_eip: u32,
    fetch_len: u32,
) -> i64 {
    let ctx = cpu.cast::<CallOutProofCtx>();
    // SAFETY: the battery passes a live CallOutProofCtx through the cpu parameter.
    unsafe {
        (*ctx).callouts += 1;
        if (*ctx).eip == u64::from(site_eip.wrapping_add(fetch_len)) {
            CLIF_CALLOUT_CONTINUE
        } else {
            CLIF_CALLOUT_EXIT
        }
    }
}

/// Fault stub: simulates a delivered-fault redirect (the interpreter retiring the call-out
/// instruction with EIP moved to the handler entry) and demands an exit.
unsafe extern "C" fn fault_shim(
    cpu: *mut CpuGsw,
    _bus: *mut core::ffi::c_void,
    _site_eip: u32,
    _fetch_len: u32,
) -> i64 {
    let ctx = cpu.cast::<CallOutProofCtx>();
    // SAFETY: as above.
    unsafe {
        (*ctx).callouts += 1;
        (*ctx).eip = SYNTHETIC_HANDLER_EIP;
    }
    CLIF_CALLOUT_EXIT
}

/// Hard-stop stub: stashes an error value (standing in for the pending CpuError) and returns
/// the hard-stop disposition the caller must observe and relay.
unsafe extern "C" fn hard_stop_shim(
    cpu: *mut CpuGsw,
    _bus: *mut core::ffi::c_void,
    _site_eip: u32,
    _fetch_len: u32,
) -> i64 {
    let ctx = cpu.cast::<CallOutProofCtx>();
    // SAFETY: as above.
    unsafe {
        (*ctx).callouts += 1;
        (*ctx).pending_error = STASHED_ERROR;
    }
    CLIF_CALLOUT_HARD_STOP
}

fn new_unit_function(index: u32) -> Function {
    Function::with_name_signature(UserFuncName::user(0, index), callout_unit_signature())
}

/// The two-slot unit (battery items 1, 3, 4): capture RSP around one ordinary call-out through
/// the table, exit immediately on any non-Continue disposition, and mark the follow-on slot
/// only on Continue.
fn build_two_slot_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(40);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let shim_sig =
        builder.import_signature(callout_shim_signature(backend.isa().default_call_conv()));

    let entry = builder.create_block();
    let proceed = builder.create_block();
    let exit = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let flags = MemFlagsData::trusted();

    let rsp_pre = builder.ins().get_stack_pointer(types::I64);
    builder.ins().store(flags, rsp_pre, ctx, 0x68);
    let shim = builder.ins().load(types::I64, flags, table, 0);
    let site = builder.ins().iconst(types::I32, i64::from(SITE_EIP));
    let len = builder.ins().iconst(types::I32, i64::from(FETCH_LEN));
    let call = builder
        .ins()
        .call_indirect(shim_sig, shim, &[ctx, bus, site, len]);
    let disposition = builder.inst_results(call)[0];
    let rsp_post = builder.ins().get_stack_pointer(types::I64);
    builder.ins().store(flags, rsp_post, ctx, 0x70);
    let is_continue = builder
        .ins()
        .icmp_imm(IntCC::Equal, disposition, CLIF_CALLOUT_CONTINUE);
    builder
        .ins()
        .brif(is_continue, proceed, &[], exit, &[disposition.into()]);

    builder.switch_to_block(proceed);
    builder.seal_block(proceed);
    let slots = builder.ins().load(types::I64, flags, ctx, 0x50);
    let slots = builder.ins().iadd_imm(slots, 1);
    builder.ins().store(flags, slots, ctx, 0x50);
    let cont = builder.ins().iconst(types::I64, CLIF_CALLOUT_CONTINUE);
    builder.ins().return_(&[cont]);

    builder.append_block_param(exit, types::I64);
    builder.switch_to_block(exit);
    builder.seal_block(exit);
    let exit_disposition = builder.block_params(exit)[0];
    builder.ins().return_(&[exit_disposition]);

    builder.finalize();
    backend.finalize(func)
}

/// The chain unit (battery item 2): C0's 500k-hop `return_call_indirect` chain, extended with
/// one ordinary call-out through the table every 1000th hop. RSP is captured at hop 1 and at
/// every hop, and around every call-out; mismatches accumulate in the context so the whole
/// chain is verified with end-state asserts only.
fn build_chain_callout_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(41);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let unit_sig = builder.import_signature(callout_unit_signature());
    let shim_sig =
        builder.import_signature(callout_shim_signature(backend.isa().default_call_conv()));

    let entry = builder.create_block();
    let first_hop = builder.create_block();
    let after_first = builder.create_block();
    let do_callout = builder.create_block();
    let after_callout = builder.create_block();
    let chain = builder.create_block();
    let resolve = builder.create_block();

    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let imm_table = builder.block_params(entry)[3];
    let flags = MemFlagsData::trusted();

    let hops = builder.ins().load(types::I64, flags, ctx, 0x00);
    let hops = builder.ins().iadd_imm(hops, 1);
    builder.ins().store(flags, hops, ctx, 0x00);
    let is_first = builder.ins().icmp_imm(IntCC::Equal, hops, 1);
    builder
        .ins()
        .brif(is_first, first_hop, &[], after_first, &[]);

    builder.switch_to_block(first_hop);
    builder.seal_block(first_hop);
    let rsp = builder.ins().get_stack_pointer(types::I64);
    builder.ins().store(flags, rsp, ctx, 0x10);
    builder.ins().jump(after_first, &[]);

    builder.switch_to_block(after_first);
    builder.seal_block(after_first);
    let rsp = builder.ins().get_stack_pointer(types::I64);
    builder.ins().store(flags, rsp, ctx, 0x18);
    let rem = builder.ins().urem_imm(hops, 1000);
    let is_callout_hop = builder.ins().icmp_imm(IntCC::Equal, rem, 0);
    builder
        .ins()
        .brif(is_callout_hop, do_callout, &[], after_callout, &[]);

    builder.switch_to_block(do_callout);
    builder.seal_block(do_callout);
    let rsp_pre = builder.ins().get_stack_pointer(types::I64);
    let shim = builder.ins().load(types::I64, flags, table, 0);
    let site = builder.ins().iconst(types::I32, i64::from(SITE_EIP));
    let len = builder.ins().iconst(types::I32, i64::from(FETCH_LEN));
    let call = builder
        .ins()
        .call_indirect(shim_sig, shim, &[ctx, bus, site, len]);
    let disposition = builder.inst_results(call)[0];
    let rsp_post = builder.ins().get_stack_pointer(types::I64);
    // Accumulate both invariants branch-free: RSP must round-trip the ordinary call, and the
    // stub must have answered Continue.
    let rsp_differs = builder.ins().icmp(IntCC::NotEqual, rsp_pre, rsp_post);
    let rsp_differs = builder.ins().uextend(types::I64, rsp_differs);
    let mismatches = builder.ins().load(types::I64, flags, ctx, 0x48);
    let mismatches = builder.ins().iadd(mismatches, rsp_differs);
    builder.ins().store(flags, mismatches, ctx, 0x48);
    let not_continue = builder
        .ins()
        .icmp_imm(IntCC::NotEqual, disposition, CLIF_CALLOUT_CONTINUE);
    let not_continue = builder.ins().uextend(types::I64, not_continue);
    let bad = builder.ins().load(types::I64, flags, ctx, 0x78);
    let bad = builder.ins().iadd(bad, not_continue);
    builder.ins().store(flags, bad, ctx, 0x78);
    builder.ins().jump(after_callout, &[]);

    builder.switch_to_block(after_callout);
    builder.seal_block(after_callout);
    let remaining = builder.ins().load(types::I64, flags, ctx, 0x20);
    let remaining = builder.ins().iadd_imm(remaining, -1);
    builder.ins().store(flags, remaining, ctx, 0x20);
    let done = builder.ins().icmp_imm(IntCC::Equal, remaining, 0);
    builder.ins().brif(done, resolve, &[], chain, &[]);

    builder.switch_to_block(chain);
    builder.seal_block(chain);
    let target = builder.ins().load(types::I64, flags, ctx, 0x28);
    builder
        .ins()
        .return_call_indirect(unit_sig, target, &[ctx, bus, table, imm_table]);

    builder.switch_to_block(resolve);
    builder.seal_block(resolve);
    let resolver = builder.ins().load(types::I64, flags, ctx, 0x30);
    builder
        .ins()
        .return_call_indirect(unit_sig, resolver, &[ctx, bus, table, imm_table]);

    builder.finalize();
    backend.finalize(func)
}

/// The resolver trampoline at the widened arity (battery item 6): a Tail-convention function
/// with the four-live-parameter signature that materializes the unresolved sentinel into the
/// context and plain-returns it as the disposition, unwinding straight back through the
/// adapter's frame.
fn build_resolver(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(42);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let sentinel = builder.ins().iconst(types::I64, UNRESOLVED_DISPOSITION);
    builder
        .ins()
        .store(MemFlagsData::trusted(), sentinel, ctx, 0x08);
    builder.ins().return_(&[sentinel]);
    builder.finalize();
    backend.finalize(func)
}

/// The immediate-table unit (battery item 5): one `load.i32` from slot 0 and one `load.i8`
/// from slot 1's low byte (the uniform 4-byte stride of design section 2.2/2.3), summed into
/// the context scratch field.
fn build_imm_load_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(43);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let imm_table = builder.block_params(entry)[3];
    let flags = MemFlagsData::trusted();

    let wide = builder.ins().load(types::I32, flags, imm_table, 0);
    let wide = builder.ins().uextend(types::I64, wide);
    let byte = builder.ins().load(types::I8, flags, imm_table, 4);
    let byte = builder.ins().uextend(types::I64, byte);
    let sum = builder.ins().iadd(wide, byte);
    builder.ins().store(flags, sum, ctx, 0x60);
    let cont = builder.ins().iconst(types::I64, CLIF_CALLOUT_CONTINUE);
    builder.ins().return_(&[cont]);
    builder.finalize();
    backend.finalize(func)
}

struct Harness {
    backend: ClifBackend,
    adapter: ClifEntryFn,
}

fn harness() -> Harness {
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let adapter = backend
        .callout_adapter()
        .expect("widened adapter compiles with zero relocations");
    Harness { backend, adapter }
}

fn enter(
    harness: &Harness,
    ctx: &mut CallOutProofCtx,
    table: &ClifCallOutTable,
    imm_table: &[u32],
    entry: *const u8,
) -> i64 {
    let mut bus_marker = 0u64;
    // SAFETY: the adapter and every entry were compiled by this harness's backend at exactly
    // the five-parameter/four-live-parameter signatures and live in sealed executable memory;
    // ctx, table, the immediate slice, and the bus marker outlive the call.
    unsafe {
        (harness.adapter)(
            std::ptr::from_mut(ctx).cast::<CpuGsw>(),
            std::ptr::from_mut(&mut bus_marker).cast(),
            std::ptr::from_ref(table),
            imm_table.as_ptr(),
            entry,
        )
    }
}

/// Battery item 1: one-hop call-out with Continue. The stub answers Continue (the EIP
/// comparison holds), the unit proceeds to its follow-on slot, and RSP is identical
/// immediately before the call and immediately after it returns.
#[test]
fn callout_proof_one_hop_continue_round_trips_rsp() {
    let mut harness = harness();
    let unit = build_two_slot_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let table = ClifCallOutTable { x87: continue_shim };
    let mut ctx = CallOutProofCtx {
        eip: u64::from(SITE_EIP + FETCH_LEN),
        ..CallOutProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, &table, &[], unit);
    assert_eq!(disposition, CLIF_CALLOUT_CONTINUE);
    assert_eq!(ctx.callouts, 1);
    assert_eq!(ctx.slots_executed, 1, "Continue must reach the next slot");
    assert_ne!(ctx.rsp_pre_callout, 0);
    assert_eq!(
        ctx.rsp_pre_callout, ctx.rsp_post_callout,
        "an ordinary call-out must round-trip RSP"
    );
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 1, the predicate's negative arm: when the stub's EIP comparison fails (the
/// context EIP is not the baked fall-through), the shim answers Exit and the unit must not
/// proceed.
#[test]
fn callout_proof_eip_mismatch_exits_without_further_slots() {
    let mut harness = harness();
    let unit = build_two_slot_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let table = ClifCallOutTable { x87: continue_shim };
    let mut ctx = CallOutProofCtx {
        eip: u64::from(SITE_EIP), // not the fall-through: models a redirected retire
        ..CallOutProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, &table, &[], unit);
    assert_eq!(disposition, CLIF_CALLOUT_EXIT);
    assert_eq!(ctx.slots_executed, 0);
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 2, the DECISIVE chain: 500,000 `return_call_indirect` hops with an ordinary
/// call-out through the table every 1000th hop. RSP holds one constant across every hop (the
/// C0 tail invariant) AND round-trips every call-out (the new ordinary-call invariant), so a
/// long run interleaving both never drifts.
#[test]
fn callout_proof_long_chain_with_interleaved_callouts_keeps_the_stack_constant() {
    const HOPS: u64 = 500_000;
    let mut harness = harness();
    let unit =
        build_chain_callout_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let resolver = build_resolver(&mut harness.backend).expect("resolver compiles");
    let table = ClifCallOutTable { x87: continue_shim };
    let mut ctx = CallOutProofCtx {
        remaining: HOPS,
        unit_addr: unit as u64,
        resolver_addr: resolver as u64,
        eip: u64::from(SITE_EIP + FETCH_LEN),
        ..CallOutProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, &table, &[], unit);
    assert_eq!(disposition, UNRESOLVED_DISPOSITION);
    assert_eq!(ctx.hops, HOPS);
    assert_eq!(ctx.disposition, UNRESOLVED_DISPOSITION as u64);
    assert_eq!(ctx.callouts, HOPS / 1000);
    assert_eq!(ctx.bad_dispositions, 0, "every stub answered Continue");
    assert_ne!(ctx.rsp_first, 0);
    assert_eq!(
        ctx.rsp_first,
        ctx.rsp_last,
        "the tail chain must reuse one frame: rsp moved {} bytes over {} hops",
        ctx.rsp_last.abs_diff(ctx.rsp_first),
        HOPS
    );
    assert_eq!(
        ctx.rsp_callout_mismatches, 0,
        "every ordinary call-out must return with RSP at its pre-call value"
    );
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 3: the fault disposition. The stub simulates a delivered-fault redirect
/// (mutating the stub EIP away from fall-through) and returns Exit; the unit must exit
/// without executing further slots, the redirected EIP preserved untouched, and RSP back at
/// its pre-call value at that return.
#[test]
fn callout_proof_fault_disposition_exits_and_preserves_the_redirected_eip() {
    let mut harness = harness();
    let unit = build_two_slot_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let table = ClifCallOutTable { x87: fault_shim };
    let mut ctx = CallOutProofCtx {
        eip: u64::from(SITE_EIP + FETCH_LEN),
        ..CallOutProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, &table, &[], unit);
    assert_eq!(disposition, CLIF_CALLOUT_EXIT);
    assert_eq!(ctx.callouts, 1);
    assert_eq!(ctx.slots_executed, 0, "the exit must skip every later slot");
    assert_eq!(
        ctx.eip, SYNTHETIC_HANDLER_EIP,
        "the unit must never re-advance or restore a sequential EIP over the handler redirect"
    );
    assert_ne!(ctx.rsp_pre_callout, 0);
    assert_eq!(
        ctx.rsp_pre_callout, ctx.rsp_post_callout,
        "the fault path must not leak stack"
    );
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 4: the hard-stop relay. The stub stashes an error value and returns
/// HardStop; the entering code observes the disposition and recovers the stash with a single
/// read, proving the error-relay path round-trips before it is wired to a real CpuError.
#[test]
fn callout_proof_hard_stop_relays_the_stashed_error() {
    let mut harness = harness();
    let unit = build_two_slot_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let table = ClifCallOutTable {
        x87: hard_stop_shim,
    };
    let mut ctx = CallOutProofCtx {
        eip: u64::from(SITE_EIP + FETCH_LEN),
        ..CallOutProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, &table, &[], unit);
    assert_eq!(disposition, CLIF_CALLOUT_HARD_STOP);
    assert_eq!(ctx.slots_executed, 0);
    // The single-read recovery the dispatcher will perform on a real pending CpuError.
    let relayed = ctx.pending_error;
    assert_eq!(relayed, STASHED_ERROR);
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 5: immediate-table loads. A unit performs `load.i32` (slot 0) and `load.i8`
/// (slot 1's low byte through the uniform 4-byte stride) from a stubbed immediate table and
/// the values round-trip, proving the descriptor-load mechanism at the same standalone tier
/// as the call-out mechanism.
#[test]
fn callout_proof_immediate_table_loads_round_trip() {
    let mut harness = harness();
    let unit = build_imm_load_unit(&mut harness.backend).expect("unit compiles, zero relocations");
    let table = ClifCallOutTable { x87: continue_shim };
    // Slot 1 deliberately carries junk above its low byte: load.i8 must read only 0xAB.
    let immediates = [0x1122_3344u32, 0xDEAD_BEABu32];
    let mut ctx = CallOutProofCtx::default();
    let disposition = enter(&harness, &mut ctx, &table, &immediates, unit);
    assert_eq!(disposition, CLIF_CALLOUT_CONTINUE);
    assert_eq!(ctx.scratch, 0x1122_3344 + 0xAB);
    assert_eq!(ctx.callouts, 0, "the immediate unit performs no call-out");
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}

/// Battery item 6: the resolver/sentinel case re-proven at the widened arity. Entering the
/// resolver directly through the five-parameter adapter yields the sentinel both as the
/// materialized context field and as the returned disposition.
#[test]
fn callout_proof_resolver_alone_yields_the_sentinel_at_the_new_arity() {
    let mut harness = harness();
    let resolver = build_resolver(&mut harness.backend).expect("resolver compiles");
    let table = ClifCallOutTable { x87: continue_shim };
    let mut ctx = CallOutProofCtx::default();
    let disposition = enter(&harness, &mut ctx, &table, &[], resolver);
    assert_eq!(disposition, UNRESOLVED_DISPOSITION);
    assert_eq!(ctx.disposition, UNRESOLVED_DISPOSITION as u64);
    assert_eq!(ctx.hops, 0);
    assert_eq!(ctx.callouts, 0);
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
}
