// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Track C C1b-pre: the bus-carrying call-out ABI (design
//! dev_docs/plans/2026-07-18-track-c1b-design.md, section 1). A compiled unit is monomorphic
//! machine code, but the interpreter pipeline a call-out must reach
//! (`cycle_no_interrupt_check::<B>`) is generic over the bus. The resolution is an opaque bus
//! pointer plus a per-call, per-`B` call-out table: the unit bakes only a compile-time-constant
//! BYTE OFFSET into the table and calls through a value LOADED from it at runtime, so the
//! zero-relocation install invariant holds by construction (a runtime load is not a
//! linker-visible relocation), and the monomorphized dispatcher supplies matching shim and bus
//! pointers fresh on every call (the table and the `&mut B` cast happen in one generic function
//! body, so type agreement is guaranteed by monomorphization itself; design section 1.4).
//!
//! C1b-pre lands the shapes and the widened adapter plus their standalone proof battery
//! (`callout_proof_test.rs`); wiring real shims and `run_clif_shell` onto them is C1b-main.

use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName, types};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::ClifBackend;
use crate::CpuGsw;

/// The three-valued call-out disposition (design section 1.6). `Continue` means the
/// instruction retired in sequence (the shim's positive fall-through predicate held) and the
/// unit proceeds to its next lowered instruction. `Exit` is a side exit with exact state
/// already materialized in `CpuGsw` (a delivered guest fault, a halt, a control transfer, a
/// paused REP). `HardStop` relays a genuine Rust-level `CpuError` the caller must recover
/// from its stash and return as `Err(..)`; it can never be treated as a plain side exit.
pub(crate) const CLIF_CALLOUT_CONTINUE: i64 = 0;
pub(crate) const CLIF_CALLOUT_EXIT: i64 = 1;
pub(crate) const CLIF_CALLOUT_HARD_STOP: i64 = 2;

/// A call-out shim's signature. `cpu`/`bus_opaque` are forwarded from the unit entry
/// unchanged; `site_eip` and `fetch_len` identify the calling slot (its guest EIP and its
/// instruction length), both compile-time-baked at the call site as STRUCTURAL unit-layout
/// data, not guest immediates, so they are exempt from the F4 immediate-baking rule the same
/// way `fetch_lens` already is. The Continue predicate needs both: the shim verifies positive
/// fall-through (`eip == site_eip + fetch_len` after the retire) rather than trusting
/// `!halted`, because a delivered architectural fault retires as `Ok` with EIP redirected to
/// the handler (design finding B1).
pub(crate) type ClifCallOutFn =
    unsafe extern "C" fn(*mut CpuGsw, *mut core::ffi::c_void, u32, u32) -> i64;

/// The call-out table: one function pointer per call-out kind, indexed by a compile-time-
/// constant slot (never reordered; a unit bakes in "slot 0 is x87", not an address).
/// Assembled fresh by the monomorphized dispatcher at every `run_clif_shell::<B>` call as a
/// plain stack local, never cached across calls: its function pointers are valid only while
/// the `&mut B` that produced the opaque bus pointer is alive on the Rust stack for THAT call
/// (design section 1.4).
#[repr(C)]
pub(crate) struct ClifCallOutTable {
    pub(crate) x87: ClifCallOutFn,
    // Future call-out kinds append here; C1b ships exactly one populated slot.
}

/// The widened dispatcher-shaped entry ABI, C1b onward (design section 1.2, the definitive
/// five-parameter arity per review finding M1): cpu, the opaque bus pointer (valid for this
/// call only), this call's monomorphized shim table, the unit's immediate table
/// (`unit.immediates.as_ptr()`, dereferenced only by the unit body's own loads), and the unit
/// entry address (the adapter's own operand). The compiled unit's `CallConv::Tail` signature
/// carries the first four as live parameters.
pub(crate) type ClifEntryFn = unsafe extern "C" fn(
    *mut CpuGsw,
    *mut core::ffi::c_void,
    *const ClifCallOutTable,
    *const u32,
    *const u8,
) -> i64;

/// The unit-side `CallConv::Tail` signature: four live parameters (cpu, bus_opaque, table,
/// imm_table), one `i64` disposition return.
pub(crate) fn callout_unit_signature() -> Signature {
    let mut sig = Signature::new(CallConv::Tail);
    for _ in 0..4 {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// The shim-side signature as CLIF sees it at a `call_indirect` site: (cpu, bus_opaque,
/// site_eip: i32, fetch_len: i32) at the host default convention (a shim is an ordinary
/// `extern "C"` Rust function, not a Tail-convention unit).
pub(crate) fn callout_shim_signature(default_call_conv: CallConv) -> Signature {
    let mut sig = Signature::new(default_call_conv);
    sig.params.push(AbiParam::new(types::I64)); // *mut CpuGsw
    sig.params.push(AbiParam::new(types::I64)); // *mut c_void bus
    sig.params.push(AbiParam::new(types::I32)); // site_eip
    sig.params.push(AbiParam::new(types::I32)); // fetch_len
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Build the widened adapter: host-default convention, five parameters per `ClifEntryFn`, one
/// ordinary `call_indirect` into the Tail unit forwarding the four live parameters opaquely,
/// returning the unit's disposition. The same shape as the C1a shell adapter
/// (`unit.rs::build_adapter_function`) at the widened arity.
fn build_callout_adapter_function(default_call_conv: CallConv) -> Function {
    let mut sig = Signature::new(default_call_conv);
    for _ in 0..5 {
        sig.params.push(AbiParam::new(types::I64));
    }
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = Function::with_name_signature(UserFuncName::user(0, 30), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(callout_unit_signature());
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let imm_table = builder.block_params(entry)[3];
    let callee = builder.block_params(entry)[4];
    let call = builder
        .ins()
        .call_indirect(sig_ref, callee, &[cpu, bus, table, imm_table]);
    let disposition = builder.inst_results(call)[0];
    builder.ins().return_(&[disposition]);
    builder.finalize();
    func
}

impl ClifBackend {
    /// The widened five-parameter adapter (design section 1.2), compiled once and reused for
    /// every C1b-onward unit entry. Coexists with the C1a shell adapter until C1b-main rewires
    /// `run_clif_shell` onto this shape.
    pub(crate) fn callout_adapter(&mut self) -> Option<ClifEntryFn> {
        if let Some(addr) = self.callout_adapter_entry {
            // SAFETY: built once at the host default convention with exactly this signature
            // and lives in sealed executable memory for the backend's lifetime.
            return Some(unsafe { std::mem::transmute::<usize, ClifEntryFn>(addr) });
        }
        let conv = self.isa().default_call_conv();
        let addr = self.finalize(build_callout_adapter_function(conv))? as usize;
        self.callout_adapter_entry = Some(addr);
        // SAFETY: as above.
        Some(unsafe { std::mem::transmute::<usize, ClifEntryFn>(addr) })
    }
}
