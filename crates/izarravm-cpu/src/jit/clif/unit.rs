// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Track C C1a: the side-exit shell unit and its dispatcher-shaped adapter (review finding
//! F-A1, resolved option B). A shell unit executes nothing natively: it reads no `CpuGsw`
//! field, writes none, and returns the one disposition C1a knows immediately. It reuses the C0
//! entry machinery exactly (`cpu_clif_unit_test.rs`'s `build_add_unit`/`build_unit_adapter` are
//! the reference shapes): a `CallConv::Tail` body callable through a host-default-convention
//! adapter via `call_indirect`, the same round trip C0 proved standalone.
//!
//! Every C1a shell is architecturally identical (no lowering exists until C1b), so ONE shell
//! body and ONE adapter are compiled lazily on first use and their addresses reused for every
//! admitted key; C1b onward compiles a distinct body per key once lowering exists.

use cranelift_codegen::ir::{AbiParam, Function, InstBuilder, Signature, UserFuncName, types};
use cranelift_codegen::isa::{CallConv, TargetIsa};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use super::ClifBackend;
use crate::CpuGsw;

/// The only disposition a C1a shell can produce: side-exit to the dispatcher, which hands the
/// unit's guest bytes to the interpreter. No other disposition exists until C1b's lowering
/// gives a unit real continue/exit outcomes.
pub(crate) const SIDE_EXIT_DISPOSITION: i64 = 0;

/// The dispatcher-shaped entry ABI for every clif unit: `*mut CpuGsw`, the unit's own entry
/// address, returning its disposition. C1a's shells are the first tenant; later sub-slices
/// widen what the callee does, not this shape.
pub(crate) type ClifEntryFn = unsafe extern "C" fn(*mut CpuGsw, *const u8) -> i64;

fn shell_signature() -> Signature {
    let mut sig = Signature::new(CallConv::Tail);
    sig.params.push(AbiParam::new(types::I64)); // *mut CpuGsw, unread by a C1a shell
    sig.returns.push(AbiParam::new(types::I64));
    sig
}

/// Build the one shared shell body: ignore the `CpuGsw` pointer entirely (nothing is lowered
/// yet, so there is nothing to read or write) and return the side-exit disposition.
fn build_shell_function() -> Function {
    let mut func = Function::with_name_signature(UserFuncName::user(0, 20), shell_signature());
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let disposition = builder.ins().iconst(types::I64, SIDE_EXIT_DISPOSITION);
    builder.ins().return_(&[disposition]);
    builder.finalize();
    func
}

/// The dispatcher-shaped adapter: host-default-convention, one `call_indirect` into the Tail
/// shell, forwarding its disposition. Identical shape to the C0 register-unit test's adapter
/// (`cpu_clif_unit_test.rs::build_unit_adapter`).
fn build_adapter_function(isa: &dyn TargetIsa) -> Function {
    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // *mut CpuGsw
    sig.params.push(AbiParam::new(types::I64)); // shell entry
    sig.returns.push(AbiParam::new(types::I64));
    let mut func = Function::with_name_signature(UserFuncName::user(0, 21), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(shell_signature());
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
    func
}

impl ClifBackend {
    /// The shared C1a shell body's installed address, compiling it on first use.
    pub(crate) fn shell_entry(&mut self) -> Option<*const u8> {
        if let Some(addr) = self.shell_entry {
            return Some(addr as *const u8);
        }
        let addr = self.finalize(build_shell_function())? as usize;
        self.shell_entry = Some(addr);
        Some(addr as *const u8)
    }

    /// The dispatcher-shaped adapter, compiled once and reused for every clif unit entry.
    pub(crate) fn adapter(&mut self) -> Option<ClifEntryFn> {
        if let Some(addr) = self.adapter_entry {
            // SAFETY: built once at the host default convention with exactly this signature
            // and lives in sealed executable memory for the backend's lifetime.
            return Some(unsafe { std::mem::transmute::<usize, ClifEntryFn>(addr) });
        }
        let isa = self.isa.clone();
        let addr = self.finalize(build_adapter_function(isa.as_ref()))? as usize;
        self.adapter_entry = Some(addr);
        // SAFETY: as above.
        Some(unsafe { std::mem::transmute::<usize, ClifEntryFn>(addr) })
    }
}
