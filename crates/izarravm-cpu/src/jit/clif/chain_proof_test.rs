// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! C1d standalone ABI/sentinel proof battery (design sections 3.3b and 4.2, review finding
//! B3's required coverage), proven before C1d-main's lowering builds on it, mirroring the
//! C0 and C1b-pre proof precedents. Every unit here uses the PRODUCTION six-parameter
//! `ClifEntryFn` adapter and the five-live-parameter `CallConv::Tail` unit signature, and
//! the sentinel hop lands in the PRODUCTION resolver trampoline through the PRODUCTION
//! sentinel descriptor (`ClifBackend::sentinel_descriptor`), not stand-ins. No real
//! `CpuGsw`/`CpuBus` is involved: the `cpu` parameter carries a `#[repr(C)]` proof context,
//! exactly as the earlier batteries did.

use core::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{Function, InstBuilder, MemFlagsData, UserFuncName, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::ClifBackend;
use super::cache::ClifUnitDescriptor;
use super::callout::{
    CLIF_CHAIN_QUOTA_EXHAUSTED, CLIF_CHAIN_UNRESOLVED, ClifCallOutTable, ClifEntryFn,
    callout_unit_signature,
};
use crate::CpuGsw;

/// A stub shim for the per-call table: no proof unit here ever calls out, so the slot only
/// needs a well-typed function pointer (mirroring the callout proof battery's stubs).
unsafe extern "C" fn unreachable_shim(
    _cpu: *mut CpuGsw,
    _bus: *mut core::ffi::c_void,
    _site_eip: u32,
    _fetch_len: u32,
) -> i64 {
    unreachable!("chain proof units never call out")
}

/// The proof context, passed through the ABI's `cpu` parameter. `#[repr(C)]` so the CLIF
/// field offsets below are stable: hops 0x00, transfers 0x08, rsp_first 0x10, rsp_last
/// 0x18, unit_addr 0x20, resume_eip 0x28, condition 0x30, taken_decrements 0x38,
/// not_taken_decrements 0x40, seen_bus 0x48, seen_table 0x50, seen_imm 0x58, seen_quota
/// 0x60, descriptor_addr 0x68.
#[repr(C)]
#[derive(Default)]
struct ChainProofCtx {
    /// Unit entries: the initial adapter entry plus one per completed transfer, so a chain
    /// entered with quota N shows exactly N (the would-be N+1th entry never runs). Doubles
    /// as the no-partial-target-effects probe: the counter is the FIRST thing a unit body
    /// does, so an exhausted edge that wrongly entered its target would overcount.
    hops: u64,
    /// Completed transfers, bumped by the edge thunk together with the tail call (Direct's
    /// `linked_transfers` analogue); the returned invariant is `transfers < quota`.
    transfers: u64,
    rsp_first: u64,
    rsp_last: u64,
    /// The chain target (the unit's own entry for the self-chain proofs).
    unit_addr: u64,
    /// The resume EIP the exhausted edge materializes BEFORE yielding.
    resume_eip: u64,
    /// The Jcc proof's runtime condition (nonzero takes the taken edge).
    condition: u64,
    taken_decrements: u64,
    not_taken_decrements: u64,
    /// The signature round-trip captures: the raw parameter values as the unit saw them.
    seen_bus: u64,
    seen_table: u64,
    seen_imm: u64,
    seen_quota: u64,
    /// The sentinel hop's landing record (the PRODUCTION sentinel descriptor's address).
    descriptor_addr: u64,
}

/// The per-edge resume EIPs the thunks materialize (structural constants, standing in for
/// the successor entry linears a real terminal would bake).
const CHAIN_TARGET_EIP: u64 = 0x0001_1000;
const TAKEN_EIP: u64 = 0x0002_2000;
const NOT_TAKEN_EIP: u64 = 0x0003_3000;
const ROUND_TRIP_DISPOSITION: i64 = 0x51;

fn new_unit_function(index: u32) -> Function {
    Function::with_name_signature(UserFuncName::user(0, index), callout_unit_signature())
}

/// The signature round-trip unit (battery item 1): records every live parameter's raw value
/// into the context and returns a recognizable disposition, proving the six-parameter outer
/// adapter delivers all five live parameters to a five-live `CallConv::Tail` unit in order.
fn build_round_trip_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(60);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let imm_table = builder.block_params(entry)[3];
    let quota = builder.block_params(entry)[4];
    let flags = MemFlagsData::trusted();
    builder.ins().store(flags, bus, ctx, 0x48);
    builder.ins().store(flags, table, ctx, 0x50);
    builder.ins().store(flags, imm_table, ctx, 0x58);
    builder.ins().store(flags, quota, ctx, 0x60);
    let disposition = builder.ins().iconst(types::I64, ROUND_TRIP_DISPOSITION);
    builder.ins().return_(&[disposition]);
    builder.finalize();
    backend.finalize(func)
}

/// The quota chain unit (battery items 2, 3, 4): a self-chaining unit whose single transfer
/// edge performs Direct's decrement-and-check-BEFORE-transfer order. Entry: bump hops
/// (the target-side-effect probe), capture RSP at hop 1 and every hop. Edge: quota -= 1; at
/// zero, materialize the resume EIP and yield `CLIF_CHAIN_QUOTA_EXHAUSTED` WITHOUT
/// transferring; otherwise bump `transfers` and `return_call_indirect` to the target with
/// the decremented quota as the fifth live argument.
fn build_quota_chain_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(61);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let unit_sig = builder.import_signature(callout_unit_signature());

    let entry = builder.create_block();
    let first_hop = builder.create_block();
    let after_first = builder.create_block();
    let exhausted = builder.create_block();
    let transfer = builder.create_block();

    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let imm_table = builder.block_params(entry)[3];
    let quota = builder.block_params(entry)[4];
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
    // The edge thunk: decrement, check, and only then transfer (Direct's
    // STACK_QUOTA -= 1; jz returning; jmp order, run.rs:1897's invariant shape).
    let next_quota = builder.ins().iadd_imm(quota, -1);
    let is_exhausted = builder.ins().icmp_imm(IntCC::Equal, next_quota, 0);
    builder
        .ins()
        .brif(is_exhausted, exhausted, &[], transfer, &[]);

    builder.switch_to_block(exhausted);
    builder.seal_block(exhausted);
    let resume = builder.ins().iconst(types::I64, CHAIN_TARGET_EIP as i64);
    builder.ins().store(flags, resume, ctx, 0x28);
    let disposition = builder.ins().iconst(types::I64, CLIF_CHAIN_QUOTA_EXHAUSTED);
    builder.ins().return_(&[disposition]);

    builder.switch_to_block(transfer);
    builder.seal_block(transfer);
    let transfers = builder.ins().load(types::I64, flags, ctx, 0x08);
    let transfers = builder.ins().iadd_imm(transfers, 1);
    builder.ins().store(flags, transfers, ctx, 0x08);
    let target = builder.ins().load(types::I64, flags, ctx, 0x20);
    builder
        .ins()
        .return_call_indirect(unit_sig, target, &[ctx, bus, table, imm_table, next_quota]);

    builder.finalize();
    backend.finalize(func)
}

/// The sentinel hop unit (battery item 5): the PRODUCTION branch-free transfer thunk shape
/// over a published descriptor address. Loads the descriptor pointer (the portal-body
/// stand-in at ctx 0x68), performs the two dependent operations at compile-time-constant
/// offsets (`entry` load, `operands` address computation), decrements the quota, and
/// tail-calls the loaded entry with the descriptor's own operands table as the forwarded
/// imm_table. No branch on the descriptor: an unresolved edge lands IN the trampoline.
fn build_sentinel_hop_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(62);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let unit_sig = builder.import_signature(callout_unit_signature());

    let entry = builder.create_block();
    let exhausted = builder.create_block();
    let transfer = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let quota = builder.block_params(entry)[4];
    let flags = MemFlagsData::trusted();

    let hops = builder.ins().load(types::I64, flags, ctx, 0x00);
    let hops = builder.ins().iadd_imm(hops, 1);
    builder.ins().store(flags, hops, ctx, 0x00);

    let next_quota = builder.ins().iadd_imm(quota, -1);
    let is_exhausted = builder.ins().icmp_imm(IntCC::Equal, next_quota, 0);
    builder
        .ins()
        .brif(is_exhausted, exhausted, &[], transfer, &[]);

    builder.switch_to_block(exhausted);
    builder.seal_block(exhausted);
    let resume = builder.ins().iconst(types::I64, CHAIN_TARGET_EIP as i64);
    builder.ins().store(flags, resume, ctx, 0x28);
    let disposition = builder.ins().iconst(types::I64, CLIF_CHAIN_QUOTA_EXHAUSTED);
    builder.ins().return_(&[disposition]);

    builder.switch_to_block(transfer);
    builder.seal_block(transfer);
    let descriptor = builder.ins().load(types::I64, flags, ctx, 0x68);
    let entry_off =
        i32::try_from(offset_of!(ClifUnitDescriptor, entry)).expect("entry offset fits");
    let operands_off =
        i64::try_from(offset_of!(ClifUnitDescriptor, operands)).expect("operands offset fits");
    let target = builder.ins().load(types::I64, flags, descriptor, entry_off);
    let imm_table = builder.ins().iadd_imm(descriptor, operands_off);
    builder
        .ins()
        .return_call_indirect(unit_sig, target, &[ctx, bus, table, imm_table, next_quota]);

    builder.finalize();
    backend.finalize(func)
}

/// The per-taken-edge `Jcc` shape (design section 4.2's B3 specification, exercised
/// structurally): a runtime condition selects between TWO transfer thunks, each with its
/// OWN quota decrement, its OWN per-edge counter, and its OWN resume-EIP materialization;
/// both edges then hop through the descriptor mechanism (here the sentinel, so the run
/// terminates deterministically in the trampoline). Exactly one edge executes per
/// traversal.
fn build_jcc_edge_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_unit_function(63);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let unit_sig = builder.import_signature(callout_unit_signature());

    let entry = builder.create_block();
    let taken = builder.create_block();
    let not_taken = builder.create_block();
    let taken_exhausted = builder.create_block();
    let taken_transfer = builder.create_block();
    let not_taken_exhausted = builder.create_block();
    let not_taken_transfer = builder.create_block();

    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let quota = builder.block_params(entry)[4];
    let flags = MemFlagsData::trusted();

    let condition = builder.ins().load(types::I64, flags, ctx, 0x30);
    builder.ins().brif(condition, taken, &[], not_taken, &[]);

    let entry_off =
        i32::try_from(offset_of!(ClifUnitDescriptor, entry)).expect("entry offset fits");
    let operands_off =
        i64::try_from(offset_of!(ClifUnitDescriptor, operands)).expect("operands offset fits");

    // The taken edge: own decrement, own counter, own EIP.
    builder.switch_to_block(taken);
    builder.seal_block(taken);
    let count = builder.ins().load(types::I64, flags, ctx, 0x38);
    let count = builder.ins().iadd_imm(count, 1);
    builder.ins().store(flags, count, ctx, 0x38);
    let resume = builder.ins().iconst(types::I64, TAKEN_EIP as i64);
    builder.ins().store(flags, resume, ctx, 0x28);
    let next_quota = builder.ins().iadd_imm(quota, -1);
    let is_exhausted = builder.ins().icmp_imm(IntCC::Equal, next_quota, 0);
    builder
        .ins()
        .brif(is_exhausted, taken_exhausted, &[], taken_transfer, &[]);

    builder.switch_to_block(taken_exhausted);
    builder.seal_block(taken_exhausted);
    let disposition = builder.ins().iconst(types::I64, CLIF_CHAIN_QUOTA_EXHAUSTED);
    builder.ins().return_(&[disposition]);

    builder.switch_to_block(taken_transfer);
    builder.seal_block(taken_transfer);
    let descriptor = builder.ins().load(types::I64, flags, ctx, 0x68);
    let target = builder.ins().load(types::I64, flags, descriptor, entry_off);
    let imm_table = builder.ins().iadd_imm(descriptor, operands_off);
    builder
        .ins()
        .return_call_indirect(unit_sig, target, &[ctx, bus, table, imm_table, next_quota]);

    // The not-taken edge: its own everything.
    builder.switch_to_block(not_taken);
    builder.seal_block(not_taken);
    let count = builder.ins().load(types::I64, flags, ctx, 0x40);
    let count = builder.ins().iadd_imm(count, 1);
    builder.ins().store(flags, count, ctx, 0x40);
    let resume = builder.ins().iconst(types::I64, NOT_TAKEN_EIP as i64);
    builder.ins().store(flags, resume, ctx, 0x28);
    let next_quota = builder.ins().iadd_imm(quota, -1);
    let is_exhausted = builder.ins().icmp_imm(IntCC::Equal, next_quota, 0);
    builder.ins().brif(
        is_exhausted,
        not_taken_exhausted,
        &[],
        not_taken_transfer,
        &[],
    );

    builder.switch_to_block(not_taken_exhausted);
    builder.seal_block(not_taken_exhausted);
    let disposition = builder.ins().iconst(types::I64, CLIF_CHAIN_QUOTA_EXHAUSTED);
    builder.ins().return_(&[disposition]);

    builder.switch_to_block(not_taken_transfer);
    builder.seal_block(not_taken_transfer);
    let descriptor = builder.ins().load(types::I64, flags, ctx, 0x68);
    let target = builder.ins().load(types::I64, flags, descriptor, entry_off);
    let imm_table = builder.ins().iadd_imm(descriptor, operands_off);
    builder
        .ins()
        .return_call_indirect(unit_sig, target, &[ctx, bus, table, imm_table, next_quota]);

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
        .expect("six-parameter adapter compiles with zero relocations");
    Harness { backend, adapter }
}

fn enter(harness: &Harness, ctx: &mut ChainProofCtx, quota: u64, entry: *const u8) -> i64 {
    let mut bus_marker = 0u64;
    let table = ClifCallOutTable {
        x87: unreachable_shim,
    };
    let imm = [0u32; 2];
    // SAFETY: the adapter and every entry were compiled by this harness's backend at
    // exactly the six-parameter/five-live-parameter signatures and live in sealed
    // executable memory; the context, table, immediate slice, and bus marker outlive the
    // call, and no proof unit ever invokes the (real) shim in the table.
    unsafe {
        (harness.adapter)(
            std::ptr::from_mut(ctx).cast::<CpuGsw>(),
            std::ptr::from_mut(&mut bus_marker).cast(),
            std::ptr::from_ref(&table),
            imm.as_ptr(),
            quota,
            entry,
        )
    }
}

/// Battery item 1: the corrected signatures round-trip. All five live parameters arrive in
/// the five-live-parameter unit in order through the six-parameter adapter, with the quota
/// value delivered exactly.
#[test]
fn chain_proof_signatures_round_trip_all_six_parameters() {
    let mut harness = harness();
    let unit = build_round_trip_unit(&mut harness.backend).expect("unit compiles");
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
    let mut ctx = ChainProofCtx::default();
    let disposition = enter(&harness, &mut ctx, 0xDEAD_BEEF, unit);
    assert_eq!(disposition, ROUND_TRIP_DISPOSITION);
    assert_eq!(
        ctx.seen_quota, 0xDEAD_BEEF,
        "the quota parameter must arrive verbatim"
    );
    assert_ne!(ctx.seen_bus, 0);
    assert_ne!(ctx.seen_table, 0);
    assert_ne!(ctx.seen_imm, 0);
}

/// Battery items 2 and 3: cross-hop quota decrement with the check-before-transfer order.
/// A chain entered with quota N enters exactly N units (no partial target-side effects at
/// the yield: the would-be N+1th entry never runs), performs exactly N - 1 transfers
/// (Direct's `linked_transfers < quota` invariant, run.rs:1897's shape), materializes the
/// resume EIP at the exhausted edge, and returns the exhaustion disposition.
#[test]
fn chain_proof_quota_decrements_and_exhausts_mid_chain() {
    let mut harness = harness();
    let unit = build_quota_chain_unit(&mut harness.backend).expect("unit compiles");
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
    for quota in [1u64, 2, 3, 17, 256] {
        let mut ctx = ChainProofCtx {
            unit_addr: unit as u64,
            ..ChainProofCtx::default()
        };
        let disposition = enter(&harness, &mut ctx, quota, unit);
        assert_eq!(disposition, CLIF_CHAIN_QUOTA_EXHAUSTED, "quota {quota}");
        assert_eq!(
            ctx.hops, quota,
            "quota {quota}: one entry per admitted unit"
        );
        assert_eq!(
            ctx.transfers,
            quota - 1,
            "quota {quota}: at most N - 1 transfers"
        );
        assert!(ctx.transfers < quota, "the run.rs:1897 invariant shape");
        assert_eq!(
            ctx.resume_eip, CHAIN_TARGET_EIP,
            "the exhausted edge materializes the un-executed target's EIP"
        );
    }
}

/// Battery item 4: the C0 500,000-hop constant-RSP assertion re-run at the widened arity
/// under preserve_frame_pointers. A genuine tail chain reuses one frame across every
/// six-parameter-adapter-entered, five-live-parameter hop; a degraded non-tail chain grows
/// the stack linearly and fails (or overflows) instead of silently passing.
#[test]
fn chain_proof_long_chain_keeps_the_stack_constant_at_the_new_arity() {
    const HOPS: u64 = 500_000;
    let mut harness = harness();
    let unit = build_quota_chain_unit(&mut harness.backend).expect("unit compiles");
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
    let mut ctx = ChainProofCtx {
        unit_addr: unit as u64,
        ..ChainProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, HOPS, unit);
    assert_eq!(disposition, CLIF_CHAIN_QUOTA_EXHAUSTED);
    assert_eq!(ctx.hops, HOPS);
    assert_eq!(ctx.transfers, HOPS - 1);
    assert_ne!(ctx.rsp_first, 0);
    assert_eq!(
        ctx.rsp_first,
        ctx.rsp_last,
        "the tail chain must reuse one frame: rsp moved {} bytes over {} hops",
        ctx.rsp_last.abs_diff(ctx.rsp_first),
        HOPS
    );
}

/// Battery item 5: the sentinel-descriptor hop. A branch-free transfer through a portal
/// publishing the PRODUCTION sentinel descriptor loads the descriptor's entry (the
/// resolver trampoline), forwards the descriptor's own zero operands table as imm_table,
/// and lands in the trampoline, which returns the unresolved disposition through the
/// adapter at the new arity.
#[test]
fn chain_proof_sentinel_hop_reaches_the_trampoline() {
    let mut harness = harness();
    let unit = build_sentinel_hop_unit(&mut harness.backend).expect("unit compiles");
    let sentinel = harness
        .backend
        .sentinel_descriptor()
        .expect("sentinel trampoline compiles with zero relocations")
        as *const ClifUnitDescriptor;
    assert_eq!(harness.backend.relocation_fallbacks(), 0);
    let mut ctx = ChainProofCtx {
        descriptor_addr: sentinel as u64,
        ..ChainProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, 8, unit);
    assert_eq!(
        disposition, CLIF_CHAIN_UNRESOLVED,
        "the hop must land in the resolver trampoline and return its disposition"
    );
    assert_eq!(ctx.hops, 1, "the trampoline is not a unit; no second entry");
    // The sentinel's address is stable across repeated lookups (Boxed per-backend storage).
    let again = harness
        .backend
        .sentinel_descriptor()
        .expect("sentinel is cached") as *const ClifUnitDescriptor;
    assert_eq!(sentinel, again);
}

/// The per-taken-edge Jcc shape (B3's specification): each edge performs its OWN decrement
/// and its OWN resume-EIP materialization, exactly one edge per traversal; exhaustion at an
/// edge yields that edge's EIP.
#[test]
fn chain_proof_jcc_edges_decrement_and_materialize_independently() {
    let mut harness = harness();
    let unit = build_jcc_edge_unit(&mut harness.backend).expect("unit compiles");
    let sentinel = harness
        .backend
        .sentinel_descriptor()
        .expect("sentinel trampoline compiles") as *const ClifUnitDescriptor
        as u64;
    assert_eq!(harness.backend.relocation_fallbacks(), 0);

    // Taken edge, quota available: transfers through the sentinel (unresolved return),
    // taken counter bumped, taken EIP materialized.
    let mut ctx = ChainProofCtx {
        descriptor_addr: sentinel,
        condition: 1,
        ..ChainProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, 4, unit);
    assert_eq!(disposition, CLIF_CHAIN_UNRESOLVED);
    assert_eq!(ctx.taken_decrements, 1);
    assert_eq!(
        ctx.not_taken_decrements, 0,
        "exactly one edge per traversal"
    );
    assert_eq!(ctx.resume_eip, TAKEN_EIP);

    // Not-taken edge, quota available.
    let mut ctx = ChainProofCtx {
        descriptor_addr: sentinel,
        condition: 0,
        ..ChainProofCtx::default()
    };
    let disposition = enter(&harness, &mut ctx, 4, unit);
    assert_eq!(disposition, CLIF_CHAIN_UNRESOLVED);
    assert_eq!(ctx.taken_decrements, 0);
    assert_eq!(ctx.not_taken_decrements, 1);
    assert_eq!(ctx.resume_eip, NOT_TAKEN_EIP);

    // Exhaustion AT an edge yields that edge's own EIP with no transfer.
    for (condition, expected_eip) in [(1u64, TAKEN_EIP), (0, NOT_TAKEN_EIP)] {
        let mut ctx = ChainProofCtx {
            descriptor_addr: sentinel,
            condition,
            ..ChainProofCtx::default()
        };
        let disposition = enter(&harness, &mut ctx, 1, unit);
        assert_eq!(disposition, CLIF_CHAIN_QUOTA_EXHAUSTED);
        assert_eq!(ctx.resume_eip, expected_eip);
    }
}
