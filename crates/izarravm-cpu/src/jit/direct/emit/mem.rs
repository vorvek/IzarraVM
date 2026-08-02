// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The read-modify-write and push-through-memory emitters, moved verbatim out of `emit.rs` to keep
//! that file under the source-line ceiling. Nothing here changed but the module boundary; every
//! item stays private to `emit`, which reaches them through `use mem::*`, and `use super::*` gives
//! this module the same view of `emit`'s private helpers it had as part of that file.

use super::*;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_rmw_inc_dec(
    e: &mut Encoder,
    is_dec: bool,
    width: MemoryWidth,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    if matches!(width, MemoryWidth::Dword) {
        emit_rmw_inc_dec_dword(e, is_dec, addr, memory, sides);
        return;
    }
    debug_assert!(matches!(width, MemoryWidth::Word));
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(e, addr, width, memory, sides, memory.address_wrap);
    emit_wide_page_guard(e, width, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // INC/DEC always changes its operand, so a watched chunk exits before any mutation.
    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        width,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.shift_r64_imm8(4, Reg::RDI, 32);
    e.or_r64_r64(Reg::RDI, Reg::RAX);
    e.store_r64_disp32(Reg::RSP, STACK_ALU_ADDRESS_KIND, Reg::RDI);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    match width {
        MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group 5 INC/DEC is word or dword")
        }
        MemoryWidth::Word => e.movzx_r32_word_disp8(Reg::RCX, Reg::RDI, 0),
        MemoryWidth::Dword => e.load_r32_disp8(Reg::RCX, Reg::RDI, 0),
    }
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    match width {
        MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group 5 INC/DEC is word or dword")
        }
        MemoryWidth::Word => {
            e.mov_r32_imm32(Reg::RDI, 1);
            e.alu_r16_r16(if is_dec { 5 } else { 0 }, Reg::RAX, Reg::RDI);
        }
        MemoryWidth::Dword => e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1),
    }
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, width, Reg::RCX, Reg::RAX);
    match width {
        MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group 5 INC/DEC is word or dword")
        }
        MemoryWidth::Word => e.store_r16_disp8(Reg::RDX, 0, Reg::RAX),
        MemoryWidth::Dword => e.store_r32_disp8(Reg::RDX, 0, Reg::RAX),
    }

    e.load_r64_disp32(Reg::RAX, Reg::RSP, STACK_ALU_ADDRESS_KIND);
    e.mov_r64_r64(Reg::RCX, Reg::RAX);
    e.shift_r64_imm8(5, Reg::RCX, 32);
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    let mode13 = e.label();
    let done = e.label();
    e.cmp_r32_imm32(Reg::RCX, u32::from(NATIVE_MODE13_KIND));
    e.jz(mode13);
    match width {
        MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group 5 INC/DEC is word or dword")
        }
        MemoryWidth::Word => emit_dynamic_word_increment(e, STACK_RAM_BYTE_WRITES),
        MemoryWidth::Dword => emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES),
    }
    e.jmp(done);
    e.place(mode13);
    match width {
        MemoryWidth::Byte | MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("group 5 INC/DEC is word or dword")
        }
        MemoryWidth::Word => {
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_READS);
            emit_dynamic_word_increment(e, STACK_MODE13_BYTE_WRITES);
        }
        MemoryWidth::Dword => {
            emit_dynamic_increment(e, STACK_MODE13_DWORD_READS);
            emit_dynamic_increment(e, STACK_MODE13_DWORD_WRITES);
        }
    }
    emit_mode13_dirty_bit(e, map);
    e.place(done);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_rmw_inc_dec_dword(
    e: &mut Encoder,
    is_dec: bool,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    sides: MemorySideExits,
) {
    let map = memory.map.expect("native RMW has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native RMW has code-watch tables");
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        sides,
        memory.address_wrap,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, sides.cross_page_or_alignment);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDI, map.read_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);

    e.load_r32_disp8(Reg::RCX, Reg::RDI, 0);
    e.mov_r32_r32(Reg::RAX, Reg::RCX);
    e.alu_r32_imm32(if is_dec { 5 } else { 0 }, Reg::RAX, 1);
    emit_capture_flags(e, ARITH_FLAGS & !crate::FLAG_CF);
    emit_pending_inc_dec(e, is_dec, MemoryWidth::Dword, Reg::RCX, Reg::RAX);
    e.store_r32_disp8(Reg::RDX, 0, Reg::RAX);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
}

/// PUSH r/m32 through memory (0xFF /6).
///
/// Both accesses refuse every page kind but plain RAM, exactly as `emit_rmw_inc_dec_dword` does.
/// That is what keeps `emit_mode13_read_completion` out of this slot: it increments the dynamic
/// mode-13 read count as soon as the read resolves, and the STORE guards below can still side
/// exit afterwards, at which point the block reports the dynamic counters against a static
/// snapshot taken before the slot. `run.rs`'s `dword_reads - exit.mode13_dword_reads` would go
/// negative, panicking a debug build and wrapping a release one into the bus charge.
///
/// A push whose source is in the mode-13 aperture therefore side exits and the interpreter runs
/// it. That is a missed lowering worth approximately nothing on this corpus, and it makes the
/// underflow unreachable rather than merely avoided by ordering.
///
/// The two accesses take DIFFERENT address wraps. The source takes the block's own
/// `memory.address_wrap`, which is Word whenever CS.D is 0; the stack cell takes `None`, because
/// the stack-width matrix has already restricted this kind to a 32-bit stack. CS.D = 0 with
/// SS.B = 1 is admissible, so the two genuinely differ.
///
/// The caller emits `sub esp, 4` AFTER this returns and after publishing the side exit. That is
/// the invariant `Push16` records: a faulting push must leave ESP at its pre-instruction value,
/// or a lazy-commit host that retries the instruction double-decrements it.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_push_mem(
    e: &mut Encoder,
    addr: DirectAddr,
    memory: MemoryEmitContext,
    source_sides: MemorySideExits,
    stack_sides: MemorySideExits,
) {
    let map = memory.map.expect("native push-mem has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native push-mem has code-watch tables");

    // The SOURCE read. RAM only, and no read-completion counter.
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        source_sides,
        memory.address_wrap,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, source_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    // The masked KIND goes to RDI. RDX must keep the RAW flags byte, because
    // `emit_read_permission_check` below consumes it. Same split `emit_rmw_inc_dec_dword` uses.
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(source_sides.unavailable_or_kind);
    // LOAD-BEARING, and its absence is a privilege bug rather than a missed lowering: without it
    // a ring-3 `push dword [supervisor_page]` reads supervisor memory natively instead of side
    // exiting to the page fault. `emit_ram_read_pointer_inner` calls this in exactly this
    // position, between the kind check and the bias lookup.
    emit_read_permission_check(e, memory.cpl3, source_sides.permission);
    emit_read_pointer(e, map, source_sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDI, Reg::RDI, 0);
    // Park it: the stack store's address and kind path clobbers RAX, RCX, RDX and RDI.
    e.store_r64_disp32(Reg::RSP, STACK_PUSH_MEM_VALUE, Reg::RDI);

    // The STACK write at SS:[ESP-4]. RAM only.
    emit_segmented_linear_address(
        e,
        stack_addr(0u32.wrapping_sub(4)),
        MemoryWidth::Dword,
        memory,
        stack_sides,
        AddressWrap::None,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, stack_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(stack_sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, stack_sides.permission);
    emit_watched_store_guard(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        stack_sides.code_watch,
    );
    // RCX MUST be recomputed here. `emit_code_watch_branch` leaves one of three watch-probe
    // intermediates in it depending on which path reached the join, and none of them is the page
    // index the bias lookup below needs. Without this the write-bias table is indexed with
    // whichever intermediate the guest's address happened to produce.
    // `emit_rmw_inc_dec_dword` recomputes immediately after its own watch join for this reason.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(stack_sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);
    e.load_r64_disp32(Reg::RDI, Reg::RSP, STACK_PUSH_MEM_VALUE);
    e.store_r32_disp8(Reg::RDX, 0, Reg::RDI);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
}
#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
pub(super) fn emit_rmw_inc_dec(
    _: &mut Encoder,
    _: bool,
    _: MemoryWidth,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

// Unlike `emit_rmw_inc_dec_dword`, whose only call site is inside `emit_rmw_inc_dec`'s own gated
// body, `emit_push_mem` is called directly from the `DirectKind::PushMem` arm in the ungated
// `emit` match, the same position `emit_store` and `emit_rmw_inc_dec` are called from. It needs
// both cfg variants for the same reason those two do.
#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
pub(super) fn emit_push_mem(
    _: &mut Encoder,
    _: DirectAddr,
    _: MemoryEmitContext,
    _: MemorySideExits,
    _: MemorySideExits,
) {
    unreachable!("direct memory lowering is x86-64-only")
}

/// CALL r/m32 through memory (0xFF /2, mod != 3). Leaves the TARGET in RDX for the caller's
/// dynamic path; the caller emits the `sub esp, 4` after publishing the side exit.
///
/// Structurally `emit_push_mem` with the two halves decoupled: there, the value read from the
/// source IS the value stored; here the source read produces the branch target and the stored
/// value is the return EIP. Both accesses stay RAM-only for `emit_push_mem`'s reason, which is a
/// COUNTER-ORDERING argument and not a conservatism: a mode-13 read completion increments the
/// dynamic mode-13 read count the moment the read resolves, and the store guards below can still
/// side exit afterwards, at which point `run.rs`'s `dword_reads - exit.mode13_dword_reads`
/// underflows. Staying RAM-only is what makes that static/dynamic pair close: the only lane either
/// access can move is the RAM write one.
///
/// The two accesses take DIFFERENT address wraps, again as in `emit_push_mem`: the operand takes
/// the block's `memory.address_wrap` (Word whenever CS.D is 0), the stack cell takes `None`
/// because the stack-width matrix has already restricted this kind to a 32-bit stack.
///
/// `cs_limit` is `Some` only for a finite CS limit. Its check runs between the target load and the
/// first store, the single position that is both after the value exists and before any guest byte
/// moves.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_call_mem(
    e: &mut Encoder,
    addr: DirectAddr,
    return_delta: u32,
    memory: MemoryEmitContext,
    source_sides: MemorySideExits,
    stack_sides: MemorySideExits,
    cs_limit: Option<(u32, Label)>,
) {
    let map = memory.map.expect("native call-mem has fast-map bases");
    let code_watch_tables = memory
        .code_watch_tables
        .expect("native call-mem has code-watch tables");

    // The SOURCE read: the branch target. RAM only, and no read-completion counter.
    emit_segmented_linear_address(
        e,
        addr,
        MemoryWidth::Dword,
        memory,
        source_sides,
        memory.address_wrap,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, source_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    // The masked KIND goes to RDI; RDX keeps the RAW flags byte for the permission check.
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(source_sides.unavailable_or_kind);
    // As in `emit_push_mem`, this is a privilege check and not bookkeeping: without it a ring-3
    // `call dword [supervisor_page]` would read supervisor memory natively.
    emit_read_permission_check(e, memory.cpl3, source_sides.permission);
    emit_read_pointer(e, map, source_sides.unavailable_or_kind);
    e.load_r32_disp8(Reg::RDI, Reg::RDI, 0);
    // BEFORE the store, so a limit refusal leaves the guest byte-for-byte untouched and the
    // interpreter's re-run reproduces the interpreter's own push-then-fault-on-next-fetch order.
    if let Some((limit, limit_exit)) = cs_limit {
        e.cmp_r32_imm32(Reg::RDI, limit);
        e.jcc(7, limit_exit);
    }
    // Park it: the stack store's address and kind path clobbers RAX, RCX, RDX and RDI.
    e.store_r64_disp32(Reg::RSP, STACK_PUSH_MEM_VALUE, Reg::RDI);

    // The STACK write of the return EIP at SS:[ESP-4]. RAM only.
    emit_segmented_linear_address(
        e,
        stack_addr(0u32.wrapping_sub(4)),
        MemoryWidth::Dword,
        memory,
        stack_sides,
        AddressWrap::None,
    );
    emit_wide_page_guard(e, MemoryWidth::Dword, stack_sides.cross_page_or_alignment);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.flags() as u64);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(stack_sides.unavailable_or_kind);
    emit_write_permission_check(e, memory.cpl3, stack_sides.permission);
    emit_watched_store_guard(
        e,
        MemoryWidth::Dword,
        map,
        code_watch_tables,
        stack_sides.code_watch,
    );
    // RCX MUST be recomputed: `emit_code_watch_branch` leaves one of three watch-probe
    // intermediates in it, none of them the page index the bias lookup needs. `emit_push_mem` and
    // `emit_rmw_inc_dec_dword` both recompute here for the same reason.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.mov_r64_imm64(Reg::RDX, map.write_biases() as u64);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(stack_sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDX, Reg::RAX);
    // The live guest EIP is still the block's entry EIP here, so `EipDelta` computes exactly the
    // `registers.eip` the interpreter pushes -- the same value and the same way `CallReg` gets it.
    emit_read_store_value(
        e,
        StoreSource::EipDelta(return_delta),
        MemoryWidth::Dword,
        Reg::RDI,
    );
    e.store_r32_disp8(Reg::RDX, 0, Reg::RDI);
    emit_dynamic_increment(e, STACK_RAM_DWORD_WRITES);
    // The target back into RDX for `emit_completed_dynamic_path`. Nothing between the park and
    // here preserved it; this is the only way to get it back.
    e.load_r64_disp32(Reg::RDX, Reg::RSP, STACK_PUSH_MEM_VALUE);
}

// Both cfg variants for the reason `emit_push_mem` needs them: this is called directly from the
// ungated `emit` match.
#[cfg(not(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
)))]
pub(super) fn emit_call_mem(
    _: &mut Encoder,
    _: DirectAddr,
    _: u32,
    _: MemoryEmitContext,
    _: MemorySideExits,
    _: MemorySideExits,
    _: Option<(u32, Label)>,
) {
    unreachable!("direct memory lowering is x86-64-only")
}
