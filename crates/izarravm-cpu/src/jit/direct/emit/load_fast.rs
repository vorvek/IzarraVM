// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one-lookup load path (`dev_docs/2026-08-07-one-lookup-load-design.md`): the lean read
//! site (D3a) replacing `emit_ram_read_pointer`'s classic pair for its ten callers, the parking
//! probe (D3b) for the Ret/Ret16/JmpMem trio, the x87 read-pointer probe (D5), and the shared
//! read-resolve stub pad (D4) — the store pad's sibling, four stubs and WIDTH-INDEPENDENT
//! because reads have no code-watch guard, the store stubs' only width-dependent front piece.
//!
//! The counter identity this file exists to preserve (design §2, the run.rs subtraction): RAM
//! reads are counted STATICALLY at compile, only mode13 reads move dynamic lanes. So the lean
//! fast RAM arm touches NO counter and NO frame slot; the inline mode13 arm moves exactly the
//! width's read lane, after every side exit its access can take; the slow arm defers to the
//! cold `emit_mode13_read_completion` over the kind the stub parked. The parking probe (trio)
//! moves nothing itself — the trio's own completion runs after their CS-limit side exit,
//! exactly as the classic front ordered it, and the RAM-kind park sits at `fast_join` where it
//! DOMINATES both native RAM arms, the untagged one and the cpl0 supervisor strip-rejoin
//! (review F1: a park on the untagged arm alone leaves every ring-0 flat-model RET reading a
//! stale, chain-surviving `STACK_READ_KIND`).
//!
//! Stub calling convention (store pad's, minus the value): RAX = linear address (preserved),
//! RDI = the probed entry on entry / the resolved host pointer on success; statuses in ECX
//! (0 success, 1 unavailable/kind, 2 permission — NO code-watch status, and a cpl0 stub cannot
//! return 2). The store side's `emit_status_dispatch` is unusable here (review F2): its tail
//! jumps to a code-watch label no read-bearing slot places, so the read dispatch below is its
//! own helper.

use super::*;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::mem::table_slot_offset;
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use super::store_fast::{append_stub, emit_stub_prologue, emit_stub_return, emit_x87_kind_park};
use crate::jit::fast_map::{
    NATIVE_LOAD_BIAS_MODE13, NATIVE_LOAD_BIAS_SUPERVISOR, NATIVE_LOAD_BIAS_TAG_MASK,
};

/// Emit the load-bias probe: page index into RCX, entry into RDI. RAX (the linear address) is
/// preserved; RDX is untouched.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_load_bias_probe(e: &mut Encoder, map: NativeMapBases) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(e, true, TABLE_SLOT_LOAD_BIASES, map.load_biases(), Reg::RDI);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
}

/// The read-resolve stubs' status dispatch at a call site: ECX carries the verdict, refusals
/// jump to THIS site's side exits. No code-watch arm (review F2 — reads have no such status and
/// no such label), and the cpl0 shape needs no compare at all: its stub's only statuses are
/// 0 and 1.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_status_dispatch(e: &mut Encoder, cpl3: bool, sides: MemorySideExits, ok: Label) {
    e.test_r32_r32(Reg::RCX, Reg::RCX);
    e.jz(ok);
    if cpl3 {
        e.cmp_r32_imm32(Reg::RCX, 1);
        e.jz(sides.unavailable_or_kind);
        e.jmp(sides.permission);
    } else {
        e.jmp(sides.unavailable_or_kind);
    }
}

/// The D3a lean site, replacing the `emit_ram_read_pointer_inner` + `emit_mode13_read_completion`
/// pair wholesale for the paired callers (design F5 composition constraint: never compose this
/// with a trailing completion — the fast RAM arm writes no frame slot, and the inline mode13 arm
/// already counted). Contract unchanged from the classic pair: RDI = host pointer, RAX
/// preserved, RCX/RDX clobbered, every side exit resolved inside.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_ram_read_pointer_fast(
    e: &mut Encoder,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
    wrap: AddressWrap,
) {
    let map = memory.map.expect("native read has fast-map bases");
    emit_segmented_linear_address(e, addr, width, memory, sides, wrap);
    if width.needs_alignment_guard() {
        emit_wide_page_guard(e, width, sides.cross_page_or_alignment);
    }
    emit_load_bias_probe(e, map);

    let aux = e.label();
    let slow = e.label();
    let stub_ok = e.label();
    let done = e.label();
    let fast_join = e.label();
    // BOTH privilege arms test both tag bits: a tagged entry's low bits are part of the VALUE,
    // so even a site allowed to read through it (cpl0 through a supervisor entry) must strip
    // them before forming the pointer — the store slice's round-one miscompile class.
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_LOAD_BIAS_MODE13 | NATIVE_LOAD_BIAS_SUPERVISOR) as u8,
    );
    e.jnz(aux);
    e.place(fast_join);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    // NO counter and NO frame-slot write: RAM reads are static (design §2). The caller's load
    // through RDI follows immediately and cannot fault.
    e.jmp(done);

    e.place(aux);
    e.cmp_r64_imm32(Reg::RDI, u32::MAX);
    e.jz(slow);
    if memory.cpl3 {
        // Ring 3 may not read a supervisor entry at all — mode13 or plain, it takes the full
        // check in the slow stub (which returns the permission status).
        e.test_r8_low_imm8(Reg::RDI, NATIVE_LOAD_BIAS_SUPERVISOR as u8);
        e.jnz(slow);
    } else {
        // Ring 0 reads through supervisor entries exactly as today's checkless cpl0 path does:
        // strip the tags and rejoin the fast load.
        let m13 = e.label();
        e.test_r8_low_imm8(Reg::RDI, NATIVE_LOAD_BIAS_MODE13 as u8);
        e.jnz(m13);
        e.and_r64_imm32(Reg::RDI, (NATIVE_LOAD_BIAS_TAG_MASK as u32) ^ u32::MAX);
        e.jmp(fast_join);
        e.place(m13);
    }
    // The mode13 arm, INLINE for the store slice's measured reason (the pad-stub transfer chain
    // is latency the instruction counts cannot see). This increment is the ONLY dynamic counter
    // on any native arm of this site, and it is legal exactly here: for every paired caller the
    // helper is the last thing in the slot that can side-exit, so the increment is after the
    // last exit (design D3a; the Ret trio, which breaks that property, uses the parking probe
    // below instead).
    e.and_r64_imm32(Reg::RDI, (NATIVE_LOAD_BIAS_TAG_MASK as u32) ^ u32::MAX);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_MODE13_DWORD_READS),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR memory reads are never 8- or 10-byte wide")
        }
    }
    e.jmp(done);

    e.place(slow);
    e.call_m64_disp32(Reg::R15, table_slot_offset(read_stub_slot_gpr(memory.cpl3)));
    emit_read_status_dispatch(e, memory.cpl3, sides, stub_ok);
    e.place(stub_ok);
    // Cold: the stub parked the kind it classified, and this is the deferred mode13 increment
    // for a stub-resolved access — the same completion the classic pair ends with, reached only
    // on stub success, after every status exit.
    emit_mode13_read_completion(e, width);
    e.place(done);
}

/// The D3b parking probe for the Ret/Ret16/JmpMem trio, emitted by
/// `emit_ram_read_pointer_inner` in place of its classic front (after the segmented address and
/// the wide guard). Parks the page KIND in `STACK_READ_KIND` and moves NO counter: the trio's
/// own `emit_mode13_read_completion` call runs after their CS-limit side exit, preserving the
/// deferred-increment ordering those sites were built around.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_read_probe_parking(
    e: &mut Encoder,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native read has fast-map bases");
    emit_load_bias_probe(e, map);

    let aux = e.label();
    let slow = e.label();
    let done = e.label();
    let fast_join = e.label();
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_LOAD_BIAS_MODE13 | NATIVE_LOAD_BIAS_SUPERVISOR) as u8,
    );
    e.jnz(aux);
    e.place(fast_join);
    // Review F1, the domination rule: the RAM-kind park lives HERE, where the untagged arm and
    // the cpl0 supervisor strip-rejoin both pass — under a ring-0 flat-model extender the
    // supervisor arm is the COMMON case, and the prologue does not zero this chain-surviving
    // slot. RDX is dead at every trio site (each reloads it from RDI after the completion).
    e.mov_r32_imm32(Reg::RDX, u32::from(NATIVE_RAM_KIND));
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.jmp(done);

    e.place(aux);
    e.cmp_r64_imm32(Reg::RDI, u32::MAX);
    e.jz(slow);
    if memory.cpl3 {
        e.test_r8_low_imm8(Reg::RDI, NATIVE_LOAD_BIAS_SUPERVISOR as u8);
        e.jnz(slow);
    } else {
        let m13 = e.label();
        e.test_r8_low_imm8(Reg::RDI, NATIVE_LOAD_BIAS_MODE13 as u8);
        e.jnz(m13);
        e.and_r64_imm32(Reg::RDI, (NATIVE_LOAD_BIAS_TAG_MASK as u32) ^ u32::MAX);
        e.jmp(fast_join);
        e.place(m13);
    }
    // The mode13 arm parks its own kind (the completion later moves the lane), strips, joins.
    e.mov_r32_imm32(Reg::RDX, u32::from(NATIVE_MODE13_KIND));
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RDX);
    e.and_r64_imm32(Reg::RDI, (NATIVE_LOAD_BIAS_TAG_MASK as u32) ^ u32::MAX);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.jmp(done);

    e.place(slow);
    e.call_m64_disp32(Reg::R15, table_slot_offset(read_stub_slot_gpr(memory.cpl3)));
    // The stub parked the kind on success; no completion here — the trio's own call runs after
    // their limit check, on every arm equally.
    emit_read_status_dispatch(e, memory.cpl3, sides, done);
    e.place(done);
}

/// The D5 x87 read-pointer probe, replacing the classify + permission + resolve + kind-pack
/// tail of `emit_x87_memory_pointer`'s read arm. ANY tagged entry — mode13, supervisor,
/// poison — takes the resolve stub at BOTH privilege levels (the lean-x87-site rule the store
/// slice paid a fixture to learn); the untouched `emit_x87_memory_completion` consumes the
/// parked pack and owns all width accounting.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_x87_read_pointer_fast(
    e: &mut Encoder,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("x87 memory block has fast-map bases");
    emit_load_bias_probe(e, map);

    let slow = e.label();
    let done = e.label();
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_LOAD_BIAS_MODE13 | NATIVE_LOAD_BIAS_SUPERVISOR) as u8,
    );
    e.jnz(slow);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    emit_x87_kind_park(e, u32::from(NATIVE_RAM_KIND));
    e.jmp(done);

    e.place(slow);
    e.call_m64_disp32(Reg::R15, table_slot_offset(read_stub_slot_x87(memory.cpl3)));
    // The stub parks the pack itself (it is the only place the kind is known).
    emit_read_status_dispatch(e, memory.cpl3, sides, done);
    e.place(done);
}

// ---------------------------------------------------------------------------------------------
// The read stub pad.

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
impl BlockCache {
    /// The read pad's published entry addresses, building it on first use (the store pad's
    /// contract verbatim). `None` -> the caller compiles the block with the inline (gate-off)
    /// read emission instead of calling through a zero slot.
    pub(crate) fn read_stub_addresses(
        &mut self,
        map: NativeMapBases,
    ) -> Option<[usize; READ_STUB_COUNT]> {
        if self.read_stub_pad.is_none() {
            let pad = emit_read_stub_pad(map);
            self.read_stub_pad = crate::jit::exec_mem::ExecutableBuffer::new(&pad.code)
                .map(|buffer| (buffer, pad.offsets));
        }
        self.read_stub_pad.as_ref().map(|(buffer, offsets)| {
            let base = buffer.entry_ptr() as usize;
            let mut addresses = [0usize; READ_STUB_COUNT];
            for (address, offset) in addresses.iter_mut().zip(offsets) {
                *address = base + offset;
            }
            addresses
        })
    }
}

/// One emitted read pad: the code bytes and each stub's offset, in the slot-layout order
/// (`read_stub_slot_gpr`, then `read_stub_slot_x87`).
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) struct ReadStubPad {
    pub(crate) code: Vec<u8>,
    pub(crate) offsets: [usize; READ_STUB_COUNT],
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(crate) fn emit_read_stub_pad(map: NativeMapBases) -> ReadStubPad {
    let mut code = Vec::new();
    let mut offsets = [0usize; READ_STUB_COUNT];
    let mut cursor = 0usize;
    for cpl3 in [false, true] {
        append_stub(
            &mut code,
            &mut offsets,
            &mut cursor,
            read_stub_slot_gpr(cpl3) - TABLE_SLOT_READ_STUBS,
            emit_gpr_read_stub(cpl3, map),
        );
    }
    for cpl3 in [false, true] {
        append_stub(
            &mut code,
            &mut offsets,
            &mut cursor,
            read_stub_slot_x87(cpl3) - TABLE_SLOT_READ_STUBS,
            emit_x87_read_stub(cpl3, map),
        );
    }
    ReadStubPad { code, offsets }
}

/// The shared classify-and-park front of both read stubs: page index from RAX, flags byte into
/// RDX (kept RAW for the permission check), kind split, unknown kinds to `status_unavailable`,
/// and the classified kind staged in ECX for the caller's park. Falls through with RCX = the
/// kind, RDX = the raw flags byte.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_read_stub_classify(e: &mut Encoder, map: NativeMapBases, status_unavailable: Label) {
    let ram = e.label();
    let staged = e.label();
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(e, true, TABLE_SLOT_FLAGS, map.flags(), Reg::RDX);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(status_unavailable);
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jmp(staged);
    e.place(ram);
    e.mov_r32_imm32(Reg::RCX, u32::from(NATIVE_RAM_KIND));
    e.place(staged);
}

/// The GPR read-resolve stub: classify, park the BARE kind in `STACK_READ_KIND` (the classic
/// front's convention at emit.rs — what `emit_mode13_read_completion` compares), permission
/// (cpl3 variant only), read-bias resolve. NO store, NO value spill (loads carry no value — the
/// store slice's F1 hazard class is structurally absent) and NO counter: the site's cold
/// completion or the trio's own completion moves the lane. Unlike the store x87 stub there is
/// no watch guard writing the aliased `STACK_WATCH_PAGE`, so the park needs no staging slot.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_gpr_read_stub(cpl3: bool, map: NativeMapBases) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);
    let status_unavailable = e.label();
    let status_permission = e.label();
    emit_read_stub_classify(&mut e, map, status_unavailable);
    // A 64-bit store of the 32-bit-materialized kind: the high half is zero, so the
    // completion's `cmp r32` against the low dword reads exactly the classic convention.
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RCX);
    emit_read_permission_check(&mut e, cpl3, status_permission);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_read_pointer(&mut e, true, map, status_unavailable);
    e.xor_r64_self(Reg::RCX);
    emit_stub_return(&mut e);

    e.place(status_unavailable);
    e.mov_r32_imm32(Reg::RCX, 1);
    emit_stub_return(&mut e);
    if cpl3 {
        e.place(status_permission);
        e.mov_r32_imm32(Reg::RCX, 2);
        emit_stub_return(&mut e);
    } else {
        // `emit_read_permission_check` emits nothing at cpl0, so the label has no referent;
        // place it on the unavailable status so the encoder's every-label-placed invariant
        // holds without dead bytes.
        e.place(status_permission);
    }
    e.finish()
}

/// The x87 read-resolve stub: the same front, parking the `kind << 32 | linear` PACK instead of
/// the bare kind — `emit_x87_memory_completion`'s convention, which splits the pack and does the
/// width-exact mode13 read accounting the moment the slot completes.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_x87_read_stub(cpl3: bool, map: NativeMapBases) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);
    let status_unavailable = e.label();
    let status_permission = e.label();
    emit_read_stub_classify(&mut e, map, status_unavailable);
    e.shift_r64_imm8(4, Reg::RCX, 32);
    e.or_r64_r64(Reg::RCX, Reg::RAX);
    e.store_r64_disp8(Reg::RSP, STACK_READ_KIND, Reg::RCX);
    emit_read_permission_check(&mut e, cpl3, status_permission);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_read_pointer(&mut e, true, map, status_unavailable);
    e.xor_r64_self(Reg::RCX);
    emit_stub_return(&mut e);

    e.place(status_unavailable);
    e.mov_r32_imm32(Reg::RCX, 1);
    emit_stub_return(&mut e);
    if cpl3 {
        e.place(status_permission);
        e.mov_r32_imm32(Reg::RCX, 2);
        emit_stub_return(&mut e);
    } else {
        e.place(status_permission);
    }
    e.finish()
}
