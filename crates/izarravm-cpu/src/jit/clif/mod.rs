// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cranelift backend skeleton (Track C slice C0). A pinned host ISA, a compile path with the
//! install-time zero-relocation invariant, and installation of finalized code into the
//! multi-span executable arena. No guest lowering lives here yet; C0 proves the embedding
//! (adapter into the tail convention, `return_call_indirect` chains, resolver trampoline)
//! standalone, and C1 builds the real unit compiler on top.
//!
//! Design contract (dev_docs/specs/2026-07-14-cranelift-backend-design.md):
//! - Host ISA feature flags pin at startup via cranelift-native detection with
//!   `opt_level=speed`, `unwind_info=false`, `is_pic=false`, so float/rounding operations
//!   select inline instructions instead of libcalls.
//! - Zero relocations is an asserted install-time invariant: a finalized buffer carrying any
//!   relocation is rejected, counted, and the address falls back to the interpreter.
//! - Helper addresses are baked in as constants and invoked with `call_indirect`; no
//!   `cranelift-module`, no `cranelift-jit`, no unwind info.
//!
//! `enable_llvm_abi_extensions` (the plan's m5 build-time question): left at its default
//! (disabled). Resolved empirically in this slice: the host-default-convention adapter calling
//! a `CallConv::Tail` callee through `call_indirect`, tail chains via `return_call_indirect`,
//! and the Tail-convention returns back through the adapter all compile and run on
//! x86_64-pc-windows-msvc without the flag. The flag gates LLVM ABI extras (i128 args,
//! Windows Fastcall variants) that C0 does not use; see the proof tests and the C0 evidence
//! note.

use std::sync::Arc;

use cranelift_codegen::control::ControlPlane;
use cranelift_codegen::ir::Function;
use cranelift_codegen::isa::TargetIsa;
use cranelift_codegen::settings::{self, Configurable};

use super::exec_mem::ExecutableArena;

/// The pinned-ISA compile-and-install seam for clif units. Owns its own multi-span arena
/// (separate from the Direct backend's block arena) so a unit's code can span multiple pages
/// through `install_span`.
pub(crate) struct ClifBackend {
    isa: Arc<dyn TargetIsa>,
    arena: ExecutableArena,
    /// Units rejected by the zero-relocation install invariant (counted fallback).
    relocation_fallbacks: u64,
    /// The six-parameter adapter's installed address (Track C C1b's call-out ABI at C1d's
    /// widened arity, design section 4.2), compiled once and reused for every unit entry.
    callout_adapter_entry: Option<usize>,
    /// The unresolved-sentinel descriptor (Track C C1d, design section 3.3b): one static
    /// descriptor per backend whose `entry` is the resolver trampoline (compiled into THIS
    /// backend's arena, hence per-backend ownership, never process-static) and whose
    /// `operands` is the all-zeros table (never loaded, but dereferenceable-shaped so the
    /// branch-free transfer thunk computes its imm_table without a special case). Boxed for
    /// address stability: portals publish this address, so it must outlive every cell that
    /// could name it and is torn down only with the backend itself (the N1(b) drop
    /// discipline). Every other field is inert filler, never read.
    sentinel: Option<Box<cache::ClifUnitDescriptor>>,
}

impl ClifBackend {
    /// Pin the host ISA once. `None` on an unsupported host or an arena allocation failure,
    /// exactly like the other native machinery: compile nothing, interpret.
    pub(crate) fn new() -> Option<Self> {
        let mut flags = settings::builder();
        flags.set("opt_level", "speed").ok()?;
        flags.set("unwind_info", "false").ok()?;
        flags.set("is_pic", "false").ok()?;
        // Required by cranelift 0.133.1's x64 tail-call implementation: emitting
        // return_call/return_call_indirect asserts preserve_frame_pointers ("frame pointers
        // aren't fundamentally required for tail calls, but the current implementation relies
        // on them being present", isa/x64/inst/emit.rs). Discovered at build time and recorded
        // in the C0 evidence alongside the llvm_abi_extensions answer.
        flags.set("preserve_frame_pointers", "true").ok()?;
        let isa = cranelift_native::builder()
            .ok()?
            .finish(settings::Flags::new(flags))
            .ok()?;
        Some(Self {
            isa,
            arena: ExecutableArena::new()?,
            relocation_fallbacks: 0,
            callout_adapter_entry: None,
            sentinel: None,
        })
    }

    /// The pinned target ISA, for building function signatures against its conventions.
    pub(crate) fn isa(&self) -> &dyn TargetIsa {
        self.isa.as_ref()
    }

    /// Compile a built CLIF function and install its finalized bytes into the arena as one
    /// span. Enforces the zero-relocation install invariant: a buffer carrying any relocation
    /// is rejected and counted, and the caller falls back to the interpreter for that address.
    pub(crate) fn finalize(&mut self, func: Function) -> Option<*const u8> {
        let mut ctx = cranelift_codegen::Context::for_function(func);
        let mut ctrl = ControlPlane::default();
        let compiled = ctx.compile(self.isa.as_ref(), &mut ctrl).ok()?;
        if !compiled.buffer.relocs().is_empty() {
            self.relocation_fallbacks = self.relocation_fallbacks.saturating_add(1);
            return None;
        }
        let code = compiled.buffer.data();
        self.arena.install_span(code)
    }

    /// How many finalized buffers the zero-relocation invariant rejected.
    #[cfg(test)]
    pub(crate) fn relocation_fallbacks(&self) -> u64 {
        self.relocation_fallbacks
    }

    /// The per-backend unresolved-sentinel descriptor (design section 3.3b), built lazily
    /// on first use: compiles the resolver trampoline into this backend's arena and wraps
    /// it in a boxed, address-stable `ClifUnitDescriptor` whose `operands` table is all
    /// zeros. Returns the stable address; `None` if the trampoline cannot compile (arena
    /// full or an unsupported shape, both of which already disable the backend's other
    /// compiles too).
    pub(crate) fn sentinel_descriptor(&mut self) -> Option<&cache::ClifUnitDescriptor> {
        if self.sentinel.is_none() {
            let entry = self.finalize(callout::build_unresolved_trampoline())? as usize;
            self.sentinel = Some(Box::new(cache::ClifUnitDescriptor::sentinel(entry)));
        }
        self.sentinel.as_deref()
    }
}

pub(crate) mod cache;
pub(crate) mod callout;
pub(crate) mod lower;

#[cfg(test)]
#[path = "proof_test.rs"]
mod proof_tests;

#[cfg(test)]
#[path = "callout_proof_test.rs"]
mod callout_proof_tests;

#[cfg(test)]
#[path = "chain_proof_test.rs"]
mod chain_proof_tests;
