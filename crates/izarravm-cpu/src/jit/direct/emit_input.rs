// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The compile-walk -> emitter handoff structs, moved verbatim out of `direct.rs` to keep that
//! file under the source-line ceiling; nothing here changed but the module boundary and the
//! `pub(super)` visibility the move requires. `direct.rs` re-exports this module's contents
//! (`use emit_input::*`), the `table_slots` precedent.

use super::*;

pub(super) struct EmitInput<'a> {
    pub(super) slots: &'a [DirectInsn],
    pub(super) span: BlockSpan,
    pub(super) raw_clocks: u32,
    pub(super) weighted_fp_clocks: u32,
    pub(super) byte_reads: u8,
    pub(super) word_reads: u8,
    pub(super) dword_reads: u8,
    pub(super) byte_stores: u8,
    pub(super) word_stores: u8,
    pub(super) dword_stores: u8,
    pub(super) self_loop: bool,
    pub(super) x87_entry_top: Option<u8>,
    pub(super) memory: MemoryEmitContext,
    pub(super) link_cell_ptrs: [usize; 2],
    /// Whether completed paths, self-loop returns and side-exit returns emit the
    /// `NativeBlockTrace` append preamble (`emit_fetch_trace`).
    ///
    /// The instrument only has a consumer when the bus wants per-block fetch observation:
    /// `run_direct_block` hands the block a `NativeExit` whose `trace_ptr` is 0 whenever
    /// `CpuBus::native_fetches_are_uniform()`, and the emitted preamble then loads the exit
    /// pointer, loads `trace_ptr`, compares it against 0 and jumps over its own body — four
    /// instructions and two dependent loads, per completed path AND per chain hop, to discover
    /// a compile-time constant. This flag moves the gate from the emitted callee to the
    /// emission site.
    ///
    /// From `JitState::native_fetch_trace`, which `try_direct_continuation` synchronises with
    /// the live bus BEFORE any probe or compile, and which `run_direct_block` re-checks as a
    /// backstop before entering native code. A change clears the block cache, so a resident
    /// block's emission shape always matches the bus that will enter it.
    pub(super) fetch_trace: bool,
}

pub(super) struct EmittedCode {
    pub(super) code: Vec<u8>,
    pub(super) body_offset: usize,
}

#[derive(Clone, Copy)]
pub(super) struct MemoryEmitContext {
    pub(super) map: Option<NativeMapBases>,
    pub(super) code_watch_tables: Option<[usize; 2]>,
    pub(super) cpl3: bool,
    /// Whether memory sites load table bases from `CpuGsw::native_table_slots`
    /// (`[r15 + disp32]`, 7 bytes) instead of baking each as a 10-byte imm64.
    /// From `JitState::r15_tables`. Both arms load the identical pointer — the
    /// publish site in the compile walk records exactly the values this
    /// context carries — so the arms differ in encoding only.
    pub(super) r15_tables: bool,
    /// Whether store emitters test the fast-map PAGE_WATCHED bit and skip the code-watch guard
    /// on unwatched pages (watched-page-bit design D3). From `JitState::watch_page_bit`. False
    /// reproduces the pre-slice emission byte for byte at the seven Group A sites; the two
    /// Group B sites keep their H6 kind-mask `and` on every arm (a behavioral no-op there,
    /// kept unconditional so the arms cannot drift), so the off arm is semantics-identical but
    /// two instructions per ALU/double-shift site larger than the pre-slice binary.
    pub(super) watch_page_bit: bool,
    /// Whether store sites emit the one-lookup probe + shared-stub shape (design D3/D4/D5)
    /// instead of the classify/resolve front. From `JitState::one_lookup_store`, AND-ed at the
    /// construction site with "the stub pad actually built" (review F5: a failed pad build
    /// falls back to the inline emission for that block) — and it requires `r15_tables`,
    /// because the stubs read every table through the R15 slots.
    pub(super) one_lookup_store: bool,
    /// Whether read sites emit the one-lookup probe (load design D3a/D3b/D5) instead of the
    /// classify/permission/resolve front. From `JitState::one_lookup_load`, AND-ed at the
    /// construction site with "the read pad actually built"; requires `r15_tables`. Fully
    /// independent of `one_lookup_store`: disjoint sites, separate pads, so an A/B of either
    /// slice leaves the other's emission untouched.
    pub(super) one_lookup_load: bool,
    pub(super) segments: SegmentLayout,
    /// Whether a ModRM-derived effective address wraps at 64K.
    ///
    /// A BLOCK property, not an address one. `decode` computes
    /// `address_size = cs.default_size_32 XOR address_size_override`, and `prefixes_supported_for`
    /// refuses the override outright, so within an admitted block the address size is a pure
    /// function of CS.D, which the mode key pins.
    ///
    /// It lives here rather than on `DirectAddr` because that struct rides inside `Load`,
    /// `Store`, `AluMemSource` and other kinds shared across many emit sites, and this
    /// property is a block-wide fact, not a per-address one.
    ///
    /// **It does NOT govern stack addresses.** Those follow SS.B, which is independent of CS.D
    /// and is keyed separately, so all nine `stack_addr` call sites pass a literal.
    pub(super) address_wrap: emit::AddressWrap,
}
