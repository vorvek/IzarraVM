// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Windows x64 unwind-info registration for the Direct JIT's code arena.
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
    /// The registered range, kept so `clear` can re-register after an arena reset.
    range_base: usize,
    range_end: usize,
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
            range_base: range_base as usize,
            range_end: range_base as usize + code_len + page_len,
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

    /// Drop every published entry (arena reset): delete the table and re-register it
    /// empty over the same range. Called only from `ExecutableArena::reset`, the
    /// clif-only path Phase 1 deletes; it exists so Phase 0 leaves no correctness hole.
    pub(crate) fn clear(&mut self) {
        if !self.table.is_null() {
            // SAFETY: `table` is the live registration created in `new` or a prior `clear`.
            unsafe { RtlDeleteGrowableFunctionTable(self.table) };
        }
        self.count = 0;
        let mut table: *mut c_void = core::ptr::null_mut();
        // SAFETY: same contract as in `new`; `entries` is unchanged and stable.
        let status = unsafe {
            RtlAddGrowableFunctionTable(
                &mut table,
                self.entries.as_ptr(),
                0,
                self.entries.len() as u32,
                self.range_base,
                self.range_end,
            )
        };
        debug_assert_eq!(status, 0, "re-registration after arena reset failed");
        // A failed re-registration leaves `table` null; `cover`/Drop both guard against
        // that (see the null checks above and in `Drop::drop`), so this degrades to an
        // unregistered arena rather than crashing.
        self.table = table;
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
