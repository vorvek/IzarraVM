// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! C0 proof battery: the Task J skeleton smoke (identity function through the pinned ISA and
//! the zero-relocation install) and the Task K load-bearing embedding proof (Rust adapter
//! into the tail convention, `return_call_indirect` self-chain, Cranelift-generated resolver
//! trampoline materializing the unresolved disposition, constant-stack criterion).

use cranelift_codegen::ir::{
    AbiParam, Function, InstBuilder, MemFlagsData, Signature, UserFuncName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::ClifBackend;

/// The proof context the tail chain mutates. `#[repr(C)]` so the CLIF field offsets below are
/// stable: hops 0x00, disposition 0x08, rsp_first 0x10, rsp_last 0x18, remaining 0x20,
/// unit_addr 0x28, resolver_addr 0x30.
#[repr(C)]
#[derive(Default)]
struct ProofCtx {
    hops: u64,
    disposition: u64,
    rsp_first: u64,
    rsp_last: u64,
    remaining: u64,
    unit_addr: u64,
    resolver_addr: u64,
}

const UNRESOLVED_DISPOSITION: u64 = 0xDEAD;

fn tail_sig() -> Signature {
    let mut sig = Signature::new(CallConv::Tail);
    sig.params.push(AbiParam::new(types::I64));
    sig
}

fn new_function(index: u32, sig: Signature) -> Function {
    Function::with_name_signature(UserFuncName::user(0, index), sig)
}

/// Unit A: bump the hop counter, capture RSP at hop 1 and at every hop (so the last capture is
/// hop N), then `return_call_indirect` to itself while iterations remain and to the resolver
/// trampoline on the final hop. Every control transfer out of this function is a tail call.
fn build_chain_unit(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_function(1, tail_sig());
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(tail_sig());

    let entry = builder.create_block();
    let first_hop = builder.create_block();
    let after_first = builder.create_block();
    let chain = builder.create_block();
    let resolve = builder.create_block();

    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let flags = MemFlagsData::trusted();

    let hops = builder.ins().load(types::I64, flags, ctx, 0x00);
    let hops = builder.ins().iadd_imm(hops, 1);
    builder.ins().store(flags, hops, ctx, 0x00);
    let is_first = builder
        .ins()
        .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, hops, 1);
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
    let remaining = builder.ins().load(types::I64, flags, ctx, 0x20);
    let remaining = builder.ins().iadd_imm(remaining, -1);
    builder.ins().store(flags, remaining, ctx, 0x20);
    let done = builder
        .ins()
        .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, remaining, 0);
    builder.ins().brif(done, resolve, &[], chain, &[]);

    builder.switch_to_block(chain);
    builder.seal_block(chain);
    let target = builder.ins().load(types::I64, flags, ctx, 0x28);
    builder.ins().return_call_indirect(sig_ref, target, &[ctx]);

    builder.switch_to_block(resolve);
    builder.seal_block(resolve);
    let resolver = builder.ins().load(types::I64, flags, ctx, 0x30);
    builder
        .ins()
        .return_call_indirect(sig_ref, resolver, &[ctx]);

    builder.finalize();
    backend.finalize(func)
}

/// The resolver trampoline: a Tail-convention function that materializes the unresolved
/// disposition into the context and plain-returns, unwinding straight back through the
/// adapter's frame (the whole tail chain reused that one frame).
fn build_resolver(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut func = new_function(2, tail_sig());
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let disposition = builder
        .ins()
        .iconst(types::I64, UNRESOLVED_DISPOSITION as i64);
    builder
        .ins()
        .store(MemFlagsData::trusted(), disposition, ctx, 0x08);
    builder.ins().return_(&[]);
    builder.finalize();
    backend.finalize(func)
}

/// The adapter: a host-default-convention function callable as Rust `extern "C"`, entering the
/// tail world with one ordinary `call_indirect` to a `CallConv::Tail` callee.
fn build_adapter(backend: &mut ClifBackend) -> Option<*const u8> {
    let mut sig = Signature::new(backend.isa().default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // ctx
    sig.params.push(AbiParam::new(types::I64)); // tail-convention entry
    let mut func = new_function(3, sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(tail_sig());
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let ctx = builder.block_params(entry)[0];
    let callee = builder.block_params(entry)[1];
    builder.ins().call_indirect(sig_ref, callee, &[ctx]);
    builder.ins().return_(&[]);
    builder.finalize();
    backend.finalize(func)
}

struct Proof {
    adapter: extern "C" fn(*mut ProofCtx, *const u8),
    unit: *const u8,
    resolver: *const u8,
}

fn build_tail_proof(backend: &mut ClifBackend) -> Proof {
    let unit = build_chain_unit(backend).expect("chain unit compiles with zero relocations");
    let resolver = build_resolver(backend).expect("resolver compiles with zero relocations");
    let adapter_ptr = build_adapter(backend).expect("adapter compiles with zero relocations");
    assert_eq!(backend.relocation_fallbacks(), 0);
    Proof {
        // SAFETY: the adapter was built at the host default calling convention with exactly
        // this two-pointer signature and lives in sealed executable memory for the backend's
        // lifetime.
        adapter: unsafe {
            std::mem::transmute::<*const u8, extern "C" fn(*mut ProofCtx, *const u8)>(adapter_ptr)
        },
        unit,
        resolver,
    }
}

fn run_chain(proof: &Proof, hops: u64) -> ProofCtx {
    let mut ctx = ProofCtx {
        remaining: hops,
        unit_addr: proof.unit as u64,
        resolver_addr: proof.resolver as u64,
        ..ProofCtx::default()
    };
    (proof.adapter)(&mut ctx, proof.unit);
    ctx
}

/// Task J smoke: identity function built with cranelift-frontend at the host convention,
/// finalized through the zero-relocation path, installed with `install_span`, and called.
#[test]
fn clif_identity_function_compiles_installs_and_runs() {
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let mut sig = Signature::new(backend.isa().default_call_conv());
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = new_function(0, sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let value = builder.block_params(entry)[0];
    builder.ins().return_(&[value]);
    builder.finalize();

    let entry_ptr = backend.finalize(func).expect("zero-relocation install");
    assert_eq!(backend.relocation_fallbacks(), 0);
    // SAFETY: built at the host default convention with exactly this signature; the span is
    // sealed executable.
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(entry_ptr) };
    assert_eq!(f(41), 41);
}

/// Task K, one-hop round trip: adapter -> tail unit A -> resolver trampoline -> back through
/// the adapter, with the unresolved disposition materialized by Cranelift-generated code.
#[test]
fn proof_one_hop_round_trip_materializes_the_unresolved_disposition() {
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let proof = build_tail_proof(&mut backend);
    let ctx = run_chain(&proof, 1);
    assert_eq!(ctx.hops, 1);
    assert_eq!(ctx.disposition, UNRESOLVED_DISPOSITION);
    assert_ne!(ctx.rsp_first, 0);
    assert_eq!(ctx.rsp_first, ctx.rsp_last);
}

/// Task K, the DECISIVE long chain: 500000 `return_call_indirect` hops through one frame.
/// A genuine tail chain reuses the frame, so RSP at hop 1 equals RSP at hop N; a degraded
/// non-tail chain grows the stack linearly (tens of bytes per frame times 500k is far past
/// the default ~1MB stack) and either fails the RSP assert or provably overflows instead of
/// silently passing.
#[test]
fn proof_long_chain_keeps_the_stack_constant() {
    const HOPS: u64 = 500_000;
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let proof = build_tail_proof(&mut backend);
    let ctx = run_chain(&proof, HOPS);
    assert_eq!(ctx.hops, HOPS);
    assert_eq!(ctx.disposition, UNRESOLVED_DISPOSITION);
    assert_ne!(ctx.rsp_first, 0);
    assert_eq!(
        ctx.rsp_first,
        ctx.rsp_last,
        "the tail chain must reuse one frame: rsp moved {} bytes over {} hops",
        ctx.rsp_last.abs_diff(ctx.rsp_first),
        HOPS
    );
}

/// The unresolved sentinel alone: entering the resolver directly through the adapter yields
/// the disposition without touching the hop machinery.
#[test]
fn proof_resolver_alone_yields_the_unresolved_sentinel() {
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let proof = build_tail_proof(&mut backend);
    let mut ctx = ProofCtx::default();
    (proof.adapter)(&mut ctx, proof.resolver);
    assert_eq!(ctx.hops, 0);
    assert_eq!(ctx.disposition, UNRESOLVED_DISPOSITION);
}
