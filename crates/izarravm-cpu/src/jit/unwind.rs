// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Windows x64 unwind-info registration for the Direct JIT's code arena.
//!
//! Direct-arena only. This module describes exactly one prologue: the Direct backend's
//! 7 `SAVED_HOST_REGS` pushes then `sub rsp, NATIVE_STACK_LEN`. (The now-removed clif
//! backend's Cranelift-compiled units had a different prologue shape -- `push rbp; mov
//! rbp, rsp` -- and never registered against this descriptor for exactly that reason.)
//!
//! Every emitted block shares ONE frame shape — the 7 `SAVED_HOST_REGS` pushes then
//! `sub rsp, NATIVE_STACK_LEN` (see direct.rs, "One frame shape for every block") — so a
//! single shared UNWIND_INFO, written once into a metadata page past the arena's code
//! bytes, serves every sealed span. Registration uses `RtlAddGrowableFunctionTable`, the
//! API built for JITs: the OS keeps the RUNTIME_FUNCTION array pointer, so the array is
//! allocated at full capacity up front and only its published count grows.
//!
//! Chained native transfers enter a target block's BODY without running its prologue.
//! That is sound here for the same reason it is sound at all: the frame shape is
//! universal, so the unwind arithmetic is identical whichever block's prologue built the
//! live frame.
//!
//! Known, accepted gap: x87-bearing blocks save RSI by a plain store (not a push), so a
//! debugger recovering nonvolatile REGISTERS through an arena frame sees a stale RSI.
//! Stack WALKS (RIP chains) — the point of this module — are unaffected.
//!
//! Known, accepted gap: five emitted `pushfq`/`popfq` (or `push`/`popfq`) pairs move RSP by
//! 8 for the span of one instruction each -- `emit_capture_flags` and `emit_load_host_flags`
//! (`jit/direct/emit.rs`), plus the three inlined `pushfq`/`pop rax` sequences in the ALU,
//! double-shift, and count-shift paths (`jit/direct/emit.rs` around lines 3614, 4141, 4212)
//! -- while the UNWIND_INFO above asserts a constant RSP for the whole block body. A sample
//! landing inside one of those narrow windows walks the frame one slot short: it reads a
//! fabricated "return address" out of the saved-RBX slot instead of the honest,
//! orphan-at-the-arena result registration didn't exist to prevent before this module. The
//! window is a handful of host instructions out of an entire block body, so the odds of a
//! sample landing there are small, and accepting it avoids the alternative of describing
//! RSP motion mid-block (`UWOP_SET_FPREG`, or capturing flags through `lahf` instead of
//! `pushfq` so RSP never moves) -- both of which change emitted bytes, which this phase
//! forbids. The fix belongs to whichever later phase is already changing the flag-capture
//! encoding (Phase 2+), not this one.
//!
//! `IZARRAVM_JIT_UNWIND=0` disables registration (A/B escape hatch). Any registration
//! failure degrades silently to the pre-registration world: code runs, walks stop at the
//! arena, nothing else changes.

use core::ffi::c_void;

use super::direct::{NATIVE_STACK_LEN, SAVED_HOST_REGS};

#[repr(C)]
struct RuntimeFunction {
    begin: u32,
    end: u32,
    unwind: u32,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlAddGrowableFunctionTable(
        dynamic_table: *mut *mut c_void,
        function_table: *const RuntimeFunction,
        entry_count: u32,
        maximum_entries: u32,
        range_base: usize,
        range_end: usize,
    ) -> i32;
    fn RtlGrowFunctionTable(dynamic_table: *mut c_void, new_entry_count: u32);
    fn RtlDeleteGrowableFunctionTable(dynamic_table: *mut c_void);
}

/// The shared UNWIND_INFO image. Layout (little-endian u16 unwind codes, reverse
/// program order):
///   header: version 1, flags 0; SizeOfProlog 0x12; CountOfCodes 9; no frame register.
///   prologue bytes it must mirror (emit.rs block prologue, byte offsets):
///     0: push rbx   1: push rbp   2: push rdi   3: push r12   5: push r13
///     7: push r14   9: push r15  11: sub rsp, imm32 (7 bytes) -> prologue ends at 0x12
///   codes: ALLOC_LARGE(form 0) of NATIVE_STACK_LEN at offset 0x12 (2 slots), then
///     PUSH_NONVOL r15,r14,r13,r12,rdi,rbp,rbx at their end offsets (7 slots).
///   9 code slots + 1 alignment pad = 24 bytes total.
const UNWIND_INFO_LEN: usize = 24;

const _: () = assert!(NATIVE_STACK_LEN.is_multiple_of(8));
const _: () = assert!((NATIVE_STACK_LEN / 8) <= 0xFFFF);
const _: () = assert!(SAVED_HOST_REGS.len() == 7);

fn unwind_info_bytes() -> [u8; UNWIND_INFO_LEN] {
    let alloc = (NATIVE_STACK_LEN / 8) as u16;
    let a = alloc.to_le_bytes();
    [
        0x01, // version 1, no flags
        0x12, // SizeOfProlog: 18 bytes, see layout comment
        0x09, // CountOfCodes
        0x00, // no frame register
        // UWOP_ALLOC_LARGE form 0: op 1, info 0, size in the NEXT slot as size/8
        0x12, 0x01, a[0], a[1],
        // UWOP_PUSH_NONVOL (op 0), info = register number, at each push's END offset
        0x0B, 0xF0, // r15
        0x09, 0xE0, // r14
        0x07, 0xD0, // r13
        0x05, 0xC0, // r12
        0x03, 0x70, // rdi
        0x02, 0x50, // rbp
        0x01, 0x30, // rbx
        0x00, 0x00, // alignment pad (unused; CountOfCodes is 9)
    ]
}

/// One growable function table covering the arena. Entries share the single
/// UNWIND_INFO written into the metadata page at `unwind_rva` past the code bytes.
pub(crate) struct ArenaUnwind {
    table: *mut c_void,
    /// Full-capacity array; the OS retains this pointer, so it must never reallocate.
    entries: Box<[RuntimeFunction]>,
    count: u32,
    unwind_rva: u32,
}

// SAFETY: the table handle and entry array are used only from the emulation thread that
// owns the arena; the OS reads the array but the pointer itself is never shared by us.
unsafe impl Send for ArenaUnwind {}
unsafe impl Sync for ArenaUnwind {}

impl ArenaUnwind {
    /// Write the shared UNWIND_INFO into `metadata_page` (one host page situated at
    /// `range_base + code_len`) and register an empty growable table for the range.
    /// Returns `None` on registration failure — the arena then simply runs unregistered.
    pub(crate) fn new(
        range_base: *const u8,
        code_len: usize,
        metadata_page: *mut u8,
        page_len: usize,
        max_entries: u32,
    ) -> Option<Self> {
        let info = unwind_info_bytes();
        // SAFETY: the caller over-allocated exactly one RW page at range_base + code_len.
        unsafe { core::ptr::copy_nonoverlapping(info.as_ptr(), metadata_page, info.len()) };
        let entries = (0..max_entries)
            .map(|_| RuntimeFunction {
                begin: 0,
                end: 0,
                unwind: 0,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut table: *mut c_void = core::ptr::null_mut();
        // SAFETY: `entries` is a stable heap allocation that outlives the table (deleted
        // in Drop before the box frees); the range covers the arena plus metadata page.
        let status = unsafe {
            RtlAddGrowableFunctionTable(
                &mut table,
                entries.as_ptr(),
                0,
                max_entries,
                range_base as usize,
                range_base as usize + code_len + page_len,
            )
        };
        (status == 0).then_some(Self {
            table,
            entries,
            count: 0,
            unwind_rva: code_len as u32,
        })
    }

    /// Cover one newly sealed span `[offset, offset + len)` (arena-relative) and publish it.
    pub(crate) fn cover(&mut self, offset: usize, len: usize) {
        if self.table.is_null() {
            return;
        }
        let Some(slot) = self.entries.get_mut(self.count as usize) else {
            return; // table full: spans past capacity stay unregistered, nothing breaks
        };
        *slot = RuntimeFunction {
            begin: offset as u32,
            end: (offset + len) as u32,
            unwind: self.unwind_rva,
        };
        self.count += 1;
        // SAFETY: `table` is a live registration over `entries`, and `count` <= capacity.
        unsafe { RtlGrowFunctionTable(self.table, self.count) };
    }
}

impl Drop for ArenaUnwind {
    fn drop(&mut self) {
        if self.table.is_null() {
            return;
        }
        // SAFETY: deleting an owned live registration; runs before `entries` is freed
        // because ExecutableArena::drop clears its unwind field before releasing memory.
        unsafe { RtlDeleteGrowableFunctionTable(self.table) };
    }
}
