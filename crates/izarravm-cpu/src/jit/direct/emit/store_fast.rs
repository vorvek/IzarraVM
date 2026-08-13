// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one-lookup store path (`dev_docs/2026-08-07-one-lookup-store-design.md`): the emitted
//! fast-store site (D3), the x87 store-pointer probe (D5), and the shared stub pad (D4).
//!
//! The site probes ONE table — `FastMapStorage::store_biases` — whose entries encode, in-band,
//! everything today's classify/permission/bias/watch front derives per store: `usize::MAX` is
//! the poison (any special page), bit 0 tags the Mode 13h aperture, bit 1 tags entries that
//! fail ring 3's user+writable test (fast for cpl0 sites only). The fast arm is
//! probe → tag test → store; everything else routes to shared stubs built once per
//! `BlockCache` in their own executable pad (the x87 re-entry pad pattern), reached by
//! `call qword [r15 + slot]`.
//!
//! Stub calling convention: RAX = linear address, RDX = store value (GPR stubs), RDI = the
//! probed entry. The page index is NOT part of the contract — every stub that needs it
//! recomputes it from RAX, so a site may clobber RCX between probe and call. **Guard 3 depends on
//! both halves of that: the site's alignment test is emitted AFTER `emit_read_store_value`, so
//! that a misaligned access entering the slow stub finds the value in RDX as the contract
//! promises, and it uses RCX as its scratch precisely because the page index is disposable.
//! Moving that test back above the value materialisation produces a store of the PREVIOUS slot's
//! value — silent, and not a fault.** Every stub's
//! prologue is
//! `pop qword [rsp + STACK_STUB_RETURN]`, which parks the return address AND restores RSP to
//! the frame level in one instruction — so every frame-offset helper (counters, watch guard,
//! dirty bit) emits at its normal displacement inside the stub — and the epilogue is
//! `jmp qword [rsp + STACK_STUB_RETURN]`. The slow stubs return a status in ECX (0 success,
//! 1 unavailable/kind, 2 permission, 3 code-watch) which the SITE dispatches to its own
//! per-site side exits, preserving exact-work exit accounting (design H4).

use super::*;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::mem::table_slot_offset;
use crate::jit::fast_map::{
    NATIVE_STORE_BIAS_MODE13, NATIVE_STORE_BIAS_SUPERVISOR, NATIVE_STORE_BIAS_TAG_MASK,
};

/// GPR store width index into the stub slot layout: byte 0, word 1, dword 2.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn gpr_width_index(width: MemoryWidth) -> usize {
    match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 1,
        MemoryWidth::Dword => 2,
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
}

/// x87 memory width index into the resolve-stub slot layout: word 0, dword 1, qword 2, tbyte 3.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn x87_width_index(width: MemoryWidth) -> usize {
    match width {
        MemoryWidth::Word => 0,
        MemoryWidth::Dword => 1,
        MemoryWidth::Qword => 2,
        MemoryWidth::Tbyte => 3,
        MemoryWidth::Byte => unreachable!("no x87 memory form is byte-wide"),
    }
}

/// Emit the store-bias probe: page index into RCX, entry into RDI. RAX (the linear address)
/// is preserved.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_store_bias_probe(e: &mut Encoder, map: NativeMapBases) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        true,
        TABLE_SLOT_STORE_BIASES,
        map.store_biases(),
        Reg::RDI,
    );
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
}

/// The slow-stub status dispatch at a call site: ECX carries the stub's verdict, and the jumps
/// target THIS site's side exits so a refused store reports the same per-site completed counts
/// the inline emission would have.
///
/// The permission branch exists only on cpl3 sites: a cpl0 stub emits no permission check and
/// can never return status 2 — and the block emitter places the permission side-exit label
/// only for cpl3 blocks (`append_stubs`), so an unconditional reference would leave an
/// unplaced label on every ring-0 block.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_status_dispatch(e: &mut Encoder, cpl3: bool, sides: MemorySideExits, done: Label) {
    e.test_r32_r32(Reg::RCX, Reg::RCX);
    e.jz(done);
    e.cmp_r32_imm32(Reg::RCX, 1);
    e.jz(sides.unavailable_or_kind);
    if cpl3 {
        e.cmp_r32_imm32(Reg::RCX, 2);
        e.jz(sides.permission);
    }
    e.jmp(sides.code_watch);
}

/// The D3 fast-store site, replacing `emit_store`'s classify + permission + bias-resolve +
/// watched-test front wholesale. Callers have already resolved `StoreSource::Selector`.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_store_fast(
    e: &mut Encoder,
    source: StoreSource,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    wrap: AddressWrap,
) {
    let map = memory.map.expect("native store has fast-map bases");
    // HOISTED above the guard, because both halves below reference `slow`. The other three labels
    // move with it so the set stays in one place.
    let aux = e.label();
    let slow = e.label();
    let done = e.label();
    let fast_join = e.label();
    emit_segmented_linear_address(e, addr, width, memory, sides, wrap);
    if width.needs_alignment_guard() {
        // The two halves are called DIRECTLY and, uniquely at this site, they are SPLIT AROUND THE
        // VALUE MATERIALISATION. The crossing half keeps its position and its RDX scratch; the
        // alignment half cannot.
        emit_page_cross_bound(e, width, sides.cross_page_or_alignment);
    }
    emit_store_bias_probe(e, map);
    // The value is materialized BEFORE the tag branch so the fast arm and both stubs share it
    // (the stubs receive it in RDX; the slow stub spills it across its own front — review F1).
    emit_read_store_value(e, source, width, Reg::RDX);
    if width.needs_alignment_guard() {
        // AFTER the value, and on RCX rather than RDX. Both halves of that are load-bearing and
        // neither failure faults.
        //
        // AFTER, because `slow` is the slow stub and the stub's contract is "RAX = linear address,
        // RDX = store value" -- it spills RDX as its SECOND instruction and reloads it to store.
        // A `jnz slow` emitted with the crossing half, three emissions above, arrives before the
        // value exists, so the stub spills and stores whatever RDX held from the PREVIOUS slot: a
        // silent wrong-value store into guest RAM.
        //
        // RCX, because RDX now holds that value and the two guard halves both use their scratch
        // destructively. RCX is free by this pad's own rule -- the page index is not part of the
        // stub contract, and every stub that needs it recomputes it from RAX. Neither the fast arm
        // nor the aux arm reads RCX after the probe, and `emit_mode13_dirty_bit` recomputes the
        // page index from RAX as its first two instructions.
        //
        // `and r32, imm32` is `81 /4 id` and `mov r32, r32` is `89 /r` at either register, so this
        // is the same four instructions at the same four encoding lengths as before the slice:
        // zero added bytes at the site.
        emit_alignment_test(e, width, Reg::RCX, slow);
    }

    // BOTH privilege arms test both tag bits: a tagged entry's low bits are part of the VALUE,
    // so even a site allowed to store through it (cpl0 through a supervisor entry) must strip
    // them before forming the pointer — an untested bit 1 would store at address+2, the
    // miscompile the WP+supervisor differential caught in round one of this suite.
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_STORE_BIAS_MODE13 | NATIVE_STORE_BIAS_SUPERVISOR) as u8,
    );
    e.jnz(aux);
    e.place(fast_join);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    // The counter clobbers RDX only after the store, exactly as today's tails do.
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    e.jmp(done);

    e.place(aux);
    e.cmp_r64_imm32(Reg::RDI, u32::MAX);
    e.jz(slow);
    if memory.cpl3 {
        // Ring 3 may not touch a supervisor entry at all — mode13 or plain, it takes the
        // full check in the slow stub.
        e.test_r8_low_imm8(Reg::RDI, NATIVE_STORE_BIAS_SUPERVISOR as u8);
        e.jnz(slow);
    } else {
        // Ring 0 stores through supervisor entries exactly as today's checkless cpl0 path
        // does: strip the tags and rejoin the fast store.
        let m13 = e.label();
        e.test_r8_low_imm8(Reg::RDI, NATIVE_STORE_BIAS_MODE13 as u8);
        e.jnz(m13);
        e.and_r64_imm32(Reg::RDI, (NATIVE_STORE_BIAS_TAG_MASK as u32) ^ u32::MAX);
        e.jmp(fast_join);
        e.place(m13);
    }
    // The mode13 arm is INLINE, not the pad stub it first shipped as: doom's aperture stores
    // are its hottest store class, and the stub's call / pop-into-slot / jmp-through-slot
    // chain measured as a ~1.7% doom-586 wall cost at byte-identical counters (pinned pairs,
    // quiet window) — indirection latency the instruction count never showed. The arm is
    // cold bytes for every site that never touches the aperture; it is the exact sequence
    // the m13 stub runs, minus the transfer.
    e.and_r64_imm32(Reg::RDI, (NATIVE_STORE_BIAS_TAG_MASK as u32) ^ u32::MAX);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    emit_mode13_dirty_bit(e, true, map);
    e.jmp(done);

    e.place(slow);
    e.call_m64_disp32(
        Reg::R15,
        table_slot_offset(store_stub_slot_slow(gpr_width_index(width), memory.cpl3)),
    );
    emit_status_dispatch(e, memory.cpl3, sides, done);
    e.place(done);
}

/// The D5 x87 store-pointer probe, replacing the classify + `emit_store_write_resolve` +
/// kind-pack tail of `emit_x87_memory_pointer`'s write arm. Emits from AFTER the segmented
/// address + wide guard; leaves RDI = host pointer and `STACK_READ_KIND` holding the same
/// `kind << 32 | linear` pack today's tail parks (the untouched completion consumes it).
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_x87_store_pointer_fast(
    e: &mut Encoder,
    width: MemoryWidth,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("x87 memory block has fast-map bases");
    emit_store_bias_probe(e, map);

    let slow = e.label();
    let done = e.label();
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_STORE_BIAS_MODE13 | NATIVE_STORE_BIAS_SUPERVISOR) as u8,
    );
    // ANY tagged entry — mode13, supervisor, poison — takes the resolve stub, at BOTH
    // privilege levels. This is deliberately narrower than the GPR site's aux arm: x87 stores
    // to the aperture or to supervisor pages are rare enough that inline arms are not worth
    // their site bytes — x87 slots emit big AVX sequences and their blocks sit closest to the
    // one-host-page split (this exact fixture class lost its tail slots to a fatter site in
    // round one) — and the stub classifies, resolves and parks every one of those cases
    // correctly (cpl0 emits no permission check inside its stub variant, today's semantics).
    e.jnz(slow);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    emit_x87_kind_park(e, u32::from(NATIVE_RAM_KIND));
    e.jmp(done);

    e.place(slow);
    e.call_m64_disp32(
        Reg::R15,
        table_slot_offset(store_stub_slot_x87(x87_width_index(width), memory.cpl3)),
    );
    // The stub parks STACK_READ_KIND itself (it is the only place the kind is known).
    emit_status_dispatch(e, memory.cpl3, sides, done);
    e.place(done);
}

/// Park `kind << 32 | linear` into `STACK_READ_KIND` — the same pack today's shared tail
/// builds from a flags re-read (emit.rs:2417-2430), built here from the statically-known kind.
/// Clobbers RCX and RDX; RAX and RDI (the resolved pointer) survive.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_x87_kind_park(e: &mut Encoder, kind: u32) {
    e.mov_r32_r32(Reg::RDX, Reg::RAX);
    e.mov_r32_imm32(Reg::RCX, kind);
    e.shift_r64_imm8(4, Reg::RCX, 32);
    e.or_r64_r64(Reg::RDX, Reg::RCX);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
}

// ---------------------------------------------------------------------------------------------
// The stub pad.

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
impl BlockCache {
    /// The stub pad's published entry addresses, building the pad on first use (the x87
    /// re-entry pad pattern: own executable mapping, never replaced). `None` when the host
    /// cannot provide an executable mapping — the caller then compiles the block with the
    /// inline (gate-off) store emission instead of calling through a zero slot (review F5).
    pub(crate) fn store_stub_addresses(
        &mut self,
        map: NativeMapBases,
        code_watch_tables: [usize; 2],
    ) -> Option<[usize; STORE_STUB_COUNT]> {
        if self.store_stub_pad.is_none() {
            let pad = emit_store_stub_pad(map, code_watch_tables);
            self.store_stub_pad = crate::jit::exec_mem::ExecutableBuffer::new(&pad.code)
                .map(|buffer| (buffer, pad.offsets));
        }
        self.store_stub_pad.as_ref().map(|(buffer, offsets)| {
            let base = buffer.entry_ptr() as usize;
            let mut addresses = [0usize; STORE_STUB_COUNT];
            for (address, offset) in addresses.iter_mut().zip(offsets) {
                *address = base + offset;
            }
            addresses
        })
    }
}

/// One emitted stub pad: the code bytes and each stub's offset, in the slot-layout order
/// (`store_stub_slot_m13`/`_slow`/`_x87`).
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) struct StoreStubPad {
    pub(crate) code: Vec<u8>,
    pub(crate) offsets: [usize; STORE_STUB_COUNT],
}

/// Emit every stub into one pad. Built once per `BlockCache`, only on the R15-tables arm
/// (asserted by the caller), so every table reference inside is a slot load; the `map` and
/// `code_watch_tables` values are threaded only to satisfy the shared helpers' imm64 arm,
/// which is dead here.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) fn emit_store_stub_pad(
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
) -> StoreStubPad {
    // Same compile-time equality the read pad asserts, and for the same reason: the deposit gate
    // encodes `alignment_mask()` while the mode13 refusal encodes `bytes() - 1`, and they agree
    // at every width this pad builds. See `emit_read_stub_pad` for why this is a const assert
    // rather than a golden byte pin.
    const _: () = assert!(
        MemoryWidth::Byte.alignment_mask() == MemoryWidth::Byte.bytes() - 1
            && MemoryWidth::Word.alignment_mask() == MemoryWidth::Word.bytes() - 1
            && MemoryWidth::Dword.alignment_mask() == MemoryWidth::Dword.bytes() - 1,
        "the store stub pad's two mask spellings must agree at every width the pad builds"
    );
    let mut code = Vec::new();
    let mut offsets = [0usize; STORE_STUB_COUNT];
    let mut cursor = 0usize;
    let widths = [MemoryWidth::Byte, MemoryWidth::Word, MemoryWidth::Dword];
    for width in widths {
        append_stub(
            &mut code,
            &mut offsets,
            &mut cursor,
            store_stub_slot_m13(gpr_width_index(width)) - TABLE_SLOT_STORE_STUBS,
            emit_m13_stub(width, map),
        );
    }
    for width in widths {
        for cpl3 in [false, true] {
            append_stub(
                &mut code,
                &mut offsets,
                &mut cursor,
                store_stub_slot_slow(gpr_width_index(width), cpl3) - TABLE_SLOT_STORE_STUBS,
                emit_slow_stub(width, cpl3, map, code_watch_tables),
            );
        }
    }
    for width in [
        MemoryWidth::Word,
        MemoryWidth::Dword,
        MemoryWidth::Qword,
        MemoryWidth::Tbyte,
    ] {
        for cpl3 in [false, true] {
            append_stub(
                &mut code,
                &mut offsets,
                &mut cursor,
                store_stub_slot_x87(x87_width_index(width), cpl3) - TABLE_SLOT_STORE_STUBS,
                emit_x87_resolve_stub(width, cpl3, map, code_watch_tables),
            );
        }
    }
    StoreStubPad { code, offsets }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn append_stub(
    code: &mut Vec<u8>,
    offsets: &mut [usize],
    cursor: &mut usize,
    index: usize,
    stub: Vec<u8>,
) {
    // 16-byte-align each entry point; the filler is int3 so a stray fall-through faults loudly.
    while !(*cursor).is_multiple_of(16) {
        code.push(0xCC);
        *cursor += 1;
    }
    offsets[index] = *cursor;
    code.extend_from_slice(&stub);
    *cursor += stub.len();
}

/// The stubs' shared prologue/epilogue: park the return address in `STACK_STUB_RETURN`,
/// restoring RSP to the frame level, and return through it.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_stub_prologue(e: &mut Encoder) {
    e.pop_m64_disp32(Reg::RSP, STACK_STUB_RETURN);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_stub_return(e: &mut Encoder) {
    e.jmp_m64_disp32(Reg::RSP, STACK_STUB_RETURN);
}

/// The mode13 fast stub: entered with a bit-0-tagged entry the SITE already admitted for its
/// privilege, so there are no refusing conditions — strip the tags, store, count, dirty-bit.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_m13_stub(width: MemoryWidth, map: NativeMapBases) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);
    e.and_r64_imm32(Reg::RDI, (NATIVE_STORE_BIAS_TAG_MASK as u32) ^ u32::MAX);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(&mut e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(&mut e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(&mut e, STACK_MODE13_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    emit_mode13_dirty_bit(&mut e, true, map);
    emit_stub_return(&mut e);
    e.finish()
}

/// The slow stub: today's full classify + permission + bias-resolve + code-watch front and
/// store tail, with side exits replaced by status returns. Entered for every poison cause —
/// unavailable kind, watched page, permission-refused-for-this-site, misaligned backing —
/// so the front re-derives everything from the flags byte exactly as the pre-slice emission
/// did, including the #711 granule-mask window on watched pages (NASCAR's win, design H6).
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_slow_stub(
    width: MemoryWidth,
    cpl3: bool,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);
    // The F1 value spill: the front below clobbers RDX four ways and the scratch set is
    // exhausted; today's inline emission survives by re-materializing from the per-site
    // `StoreSource` after the resolve, which a shared stub does not have.
    e.store_r64_disp32(Reg::RSP, STACK_PUSH_MEM_VALUE, Reg::RDX);

    let ram = e.label();
    let mode13 = e.label();
    let store = e.label();
    let status_unavailable = e.label();
    let status_permission = e.label();
    let status_watch = e.label();

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(&mut e, true, TABLE_SLOT_FLAGS, map.flags(), Reg::RDX);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    e.jmp(status_unavailable);

    // Both kind arms share the resolve; the store tail re-splits on the kind, parked in the
    // upper word of the same pack `emit_rmw_inc_dec` uses for exactly this purpose.
    e.place(ram);
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_RAM_KIND));
    e.jmp(store);
    e.place(mode13);
    // Mode 13h keeps REFUSING a misaligned access, and WHERE this sits is the whole content of it.
    //
    // The counting READ stub has separate mode13 and RAM tails, so its refusal drops naturally
    // into the mode13 one. This stub has no such tail: both kind arms fall into the shared `store`
    // label below and the kind is re-split only AFTER the write. A refusal placed "in the mode13
    // tail" by analogy with the read side would therefore land after the aperture byte had already
    // been written -- and the block would then side-exit, so the interpreter re-executes the
    // instruction and writes it a SECOND time. A double write to the aperture, caused by the very
    // mitigation meant to keep the aperture off this path.
    //
    // It also sits BEFORE `emit_write_permission_check`, matching where the pre-slice
    // `emit_wide_page_guard` sat relative to every check: refusing later would re-attribute a cpl3
    // supervisor-aperture case from alignment to Permission.
    //
    // Status 1, so this lands on the site's `unavailable_or_kind` exit rather than
    // `cross_page_or_alignment`. Deliberate and counters-only; see the read stub for the size
    // argument against a dedicated status.
    //
    // Wrapped, because the pad builds a stub for every GPR width including Byte.
    if width.needs_alignment_guard() {
        // RAX is the linear address and this stub's front preserves it by contract.
        //
        // `bytes() - 1`, NOT `alignment_mask()`, for the reason spelled out at the counting read
        // stub's matching refusal: this gate asks "is this one natural transaction of its full
        // width", the aperture question, while the deposit gate further down this pad asks the
        // call-site guard's question and reads `alignment_mask()`.
        //
        // A SMALLER mask refuses FEWER accesses, so substituting the guard's mask here would
        // weaken the refusal for wide widths rather than tighten it. Getting this gate wrong once
        // already produced a double write to the aperture; widening the pad means revisiting it.
        e.test_r8_low_imm8(Reg::RAX, (width.bytes() - 1) as u8);
        e.jnz(status_unavailable);
    }
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.place(store);
    e.shift_r64_imm8(4, Reg::RCX, 32);
    e.or_r64_r64(Reg::RCX, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RCX);

    emit_write_permission_check(&mut e, cpl3, status_permission);
    // The pointer resolve needs the page index back in RCX (the permission check consumed the
    // pack staging in RCX only up to the spill above).
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(&mut e, true, map, status_unavailable);
    emit_watched_store_guard(&mut e, width, true, map, code_watch_tables, status_watch);

    // Store, then the counters for whichever kind the classify found.
    e.load_r64_disp32(Reg::RDX, Reg::RSP, STACK_PUSH_MEM_VALUE);
    match width {
        MemoryWidth::Byte => e.store_r8_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDI, 0, Reg::RDX),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    let count_mode13 = e.label();
    let counted = e.label();
    e.load_r64_disp32(Reg::RCX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(count_mode13);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(&mut e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(&mut e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(&mut e, STACK_RAM_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    // The misaligned RAM store this stub now SERVES rather than refuses: charge the extra byte
    // cycles it owes beyond the one wide cycle the increment above accounts for. Placed here, in
    // the RAM counter arm, because by this point the store has landed and the code-watch guard has
    // passed -- the access is committed and can no longer refuse.
    //
    // CONDITIONAL, and on this side that is not a formality. Everything the site cannot serve
    // inline arrives here: poisoned entries, supervisor entries at cpl3, and -- the population
    // that matters -- every store to a WATCHED page, because a watch edge poisons the page's store
    // bias. Those are ordinary ALIGNED stores and they must charge exactly what they charged
    // before this slice. An unconditional deposit over-charges all of them.
    //
    // RAX is still the linear address: the front preserves it by contract and the store above went
    // through RDI. RDX holds the reloaded store value and is dead from here.
    if width.needs_alignment_guard() {
        let aligned = e.label();
        // The GUARD's mask, deliberately: this gate asks "did the call-site alignment test
        // consider this misaligned", because that test is the only alignment-caused route into
        // this stub, and the deposit must charge exactly the accesses it refused. Contrast the
        // mode13 refusal near the top of this pad, which asks a different question and keeps
        // `bytes() - 1`.
        e.test_r8_low_imm8(Reg::RAX, width.alignment_mask() as u8);
        e.jz(aligned);
        // Gate and deposit are read from the same width model on purpose. They agree for the
        // three widths this pad builds; for a wide width they would not, and a site that gated on
        // one model and charged from the other would decide "split" on a 4-byte criterion and
        // then bill an 8-byte penalty.
        emit_dynamic_split_extra(&mut e, width.split_extra_bytes());
        e.place(aligned);
    }
    e.xor_r64_self(Reg::RCX);
    e.jmp(counted);
    e.place(count_mode13);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(&mut e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Word => emit_dynamic_word_increment(&mut e, STACK_MODE13_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(&mut e, STACK_MODE13_DWORD_WRITES),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR stores are never 8- or 10-byte wide")
        }
    }
    emit_mode13_dirty_bit(&mut e, true, map);
    e.xor_r64_self(Reg::RCX);
    e.place(counted);
    emit_stub_return(&mut e);

    e.place(status_unavailable);
    e.mov_r32_imm32(Reg::RCX, 1);
    emit_stub_return(&mut e);
    e.place(status_permission);
    e.mov_r32_imm32(Reg::RCX, 2);
    emit_stub_return(&mut e);
    e.place(status_watch);
    e.mov_r32_imm32(Reg::RCX, 3);
    emit_stub_return(&mut e);
    e.finish()
}

/// The x87 resolve-only stub: the same front, NO store and NO counter (the x87 emitter and
/// its completion own those, keyed off the `STACK_READ_KIND` pack this stub parks on
/// success). Returns RDI = the resolved host pointer.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_resolve_stub(
    width: MemoryWidth,
    cpl3: bool,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);

    let ram = e.label();
    let mode13 = e.label();
    let resolve = e.label();
    let status_unavailable = e.label();
    let status_permission = e.label();
    let status_watch = e.label();

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(&mut e, true, TABLE_SLOT_FLAGS, map.flags(), Reg::RDX);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    e.jmp(status_unavailable);

    e.place(ram);
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_RAM_KIND));
    e.jmp(resolve);
    e.place(mode13);
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.place(resolve);
    e.shift_r64_imm8(4, Reg::RCX, 32);
    e.or_r64_r64(Reg::RCX, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RCX);

    emit_write_permission_check(&mut e, cpl3, status_permission);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_write_pointer(&mut e, true, map, status_unavailable);
    emit_watched_store_guard(&mut e, width, true, map, code_watch_tables, status_watch);

    // Success: move the parked pack into STACK_READ_KIND for the completion, zero the status.
    e.load_r64_disp32(Reg::RDX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
    e.xor_r64_self(Reg::RCX);
    emit_stub_return(&mut e);

    e.place(status_unavailable);
    e.mov_r32_imm32(Reg::RCX, 1);
    emit_stub_return(&mut e);
    e.place(status_permission);
    e.mov_r32_imm32(Reg::RCX, 2);
    emit_stub_return(&mut e);
    e.place(status_watch);
    e.mov_r32_imm32(Reg::RCX, 3);
    emit_stub_return(&mut e);
    e.finish()
}
