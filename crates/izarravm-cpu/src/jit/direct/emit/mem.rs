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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    let valid = e.label();
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(valid);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(sides.unavailable_or_kind);
    e.place(valid);
    if memory.watch_page_bit {
        // D3's carry shape: RCX is dead here (the page index is recomputed after the join), so
        // it carries the PAGE_WATCHED bit across the cpl3 permission check, which destroys the
        // flags byte in RDX (H3).
        e.mov_r32_r32(Reg::RCX, Reg::RDX);
        e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_PAGE_WATCHED));
    }
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    // INC/DEC always changes its operand, so a watched chunk exits before any mutation.
    let unwatched = e.label();
    if memory.watch_page_bit {
        // The permission check clobbered host flags, so re-test the carried bit explicitly.
        e.cmp_r32_imm32(Reg::RCX, 0);
        e.jz(unwatched);
    }
    emit_code_watch_branch(
        e,
        width,
        memory.r15_tables,
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_READ_BIASES,
        map.read_biases(),
        Reg::RDI,
    );
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_WRITE_BIASES,
        map.write_biases(),
        Reg::RDX,
    );
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
    emit_mode13_dirty_bit(e, memory.r15_tables, map);
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(sides.unavailable_or_kind);
    if memory.watch_page_bit {
        // D3's carry shape, as in the word arm above: RCX is recomputed after the join.
        e.mov_r32_r32(Reg::RCX, Reg::RDX);
        e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_PAGE_WATCHED));
    }
    emit_write_permission_check(e, memory.cpl3, sides.permission);

    let unwatched = e.label();
    if memory.watch_page_bit {
        e.cmp_r32_imm32(Reg::RCX, 0);
        e.jz(unwatched);
    }
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        memory.r15_tables,
        map,
        code_watch_tables,
        sides.code_watch,
        unwatched,
    );
    e.place(unwatched);

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_READ_BIASES,
        map.read_biases(),
        Reg::RDI,
    );
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(sides.unavailable_or_kind);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_WRITE_BIASES,
        map.write_biases(),
        Reg::RDX,
    );
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
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
    emit_read_pointer(e, memory.r15_tables, map, source_sides.unavailable_or_kind);
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(stack_sides.unavailable_or_kind);
    if memory.watch_page_bit {
        // D3's carry shape: RCX is recomputed after the join either way.
        e.mov_r32_r32(Reg::RCX, Reg::RDX);
        e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_PAGE_WATCHED));
    }
    emit_write_permission_check(e, memory.cpl3, stack_sides.permission);
    let stack_unwatched = e.label();
    if memory.watch_page_bit {
        e.cmp_r32_imm32(Reg::RCX, 0);
        e.jz(stack_unwatched);
    }
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        memory.r15_tables,
        map,
        code_watch_tables,
        stack_sides.code_watch,
        stack_unwatched,
    );
    e.place(stack_unwatched);
    // RCX MUST be recomputed here. `emit_code_watch_branch` leaves one of three watch-probe
    // intermediates in it depending on which path reached the join (and the D3 skip path leaves
    // the carried bit), and none of them is the page index the bias lookup below needs. Without
    // this the write-bias table is indexed with whichever intermediate the guest's address
    // happened to produce.
    // `emit_rmw_inc_dec_dword` recomputes immediately after its own watch join for this reason.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_WRITE_BIASES,
        map.write_biases(),
        Reg::RDX,
    );
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    // The masked KIND goes to RDI; RDX keeps the RAW flags byte for the permission check.
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(source_sides.unavailable_or_kind);
    // As in `emit_push_mem`, this is a privilege check and not bookkeeping: without it a ring-3
    // `call dword [supervisor_page]` would read supervisor memory natively.
    emit_read_permission_check(e, memory.cpl3, source_sides.permission);
    emit_read_pointer(e, memory.r15_tables, map, source_sides.unavailable_or_kind);
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
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_FLAGS,
        map.flags(),
        Reg::RDX,
    );
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jnz(stack_sides.unavailable_or_kind);
    if memory.watch_page_bit {
        // D3's carry shape, exactly as `emit_push_mem`'s stack write.
        e.mov_r32_r32(Reg::RCX, Reg::RDX);
        e.and_r32_imm32(Reg::RCX, u32::from(NATIVE_PAGE_WATCHED));
    }
    emit_write_permission_check(e, memory.cpl3, stack_sides.permission);
    let stack_unwatched = e.label();
    if memory.watch_page_bit {
        e.cmp_r32_imm32(Reg::RCX, 0);
        e.jz(stack_unwatched);
    }
    emit_code_watch_branch(
        e,
        MemoryWidth::Dword,
        memory.r15_tables,
        map,
        code_watch_tables,
        stack_sides.code_watch,
        stack_unwatched,
    );
    e.place(stack_unwatched);
    // RCX MUST be recomputed: `emit_code_watch_branch` leaves one of three watch-probe
    // intermediates in it (and the D3 skip path leaves the carried bit), none of them the page
    // index the bias lookup needs. `emit_push_mem` and `emit_rmw_inc_dec_dword` both recompute
    // here for the same reason.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        memory.r15_tables,
        TABLE_SLOT_WRITE_BIASES,
        map.write_biases(),
        Reg::RDX,
    );
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

// The table-base and code-watch emission helpers, moved verbatim from emit.rs for the same
// source-line-ceiling reason as the rest of this file; pub(super) so emit.rs keeps reaching
// them through use mem::*.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_read_pointer(
    e: &mut Encoder,
    r15_tables: bool,
    map: NativeMapBases,
    side: Label,
) {
    emit_table_base(
        e,
        r15_tables,
        TABLE_SLOT_READ_BIASES,
        map.read_biases(),
        Reg::RDI,
    );
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_write_pointer(
    e: &mut Encoder,
    r15_tables: bool,
    map: NativeMapBases,
    side: Label,
) {
    emit_table_base(
        e,
        r15_tables,
        TABLE_SLOT_WRITE_BIASES,
        map.write_biases(),
        Reg::RDI,
    );
    e.load_r64_sib_scale8(Reg::RDI, Reg::RDI, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDI, NATIVE_UNAVAILABLE_BIAS as u32);
    e.jz(side);
    e.add_r64_r64(Reg::RDI, Reg::RAX);
}

/// Load a table base into `dst`: R15-relative from `CpuGsw::native_table_slots`
/// (7 bytes, hot L1) on the R15 arm, the baked 10-byte immediate otherwise.
/// Identical pointer either way — the compile walk publishes exactly the values
/// its `MemoryEmitContext` carries, before the block can be installed.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_table_base(
    e: &mut Encoder,
    r15_tables: bool,
    slot: usize,
    value: usize,
    dst: Reg,
) {
    if r15_tables {
        e.load_r64_disp32(dst, Reg::R15, table_slot_offset(slot));
    } else {
        e.mov_r64_imm64(dst, value as u64);
    }
}

/// Offset of `CpuGsw::native_table_slots[slot]` from R15, computed the way
/// `eip_offset` and friends are so the field's position is load-bearing only
/// through `offset_of!`.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn table_slot_offset(slot: usize) -> i32 {
    (core::mem::offset_of!(CpuGsw, native_table_slots)
        + core::mem::offset_of!(NativeTableSlots, slots)
        + slot * core::mem::size_of::<usize>()) as i32
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_watched_store_guard(
    e: &mut Encoder,
    width: MemoryWidth,
    r15_tables: bool,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: Label,
) {
    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        width,
        r15_tables,
        map,
        code_watch_tables,
        side,
        unwatched,
    );
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_watched_alu_result_guard(
    e: &mut Encoder,
    width: MemoryWidth,
    r15_tables: bool,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    side: Label,
) {
    let unwatched = e.label();
    emit_code_watch_branch(
        e,
        width,
        r15_tables,
        map,
        code_watch_tables,
        side,
        unwatched,
    );
    e.place(unwatched);
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_code_watch_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    r15_tables: bool,
    map: NativeMapBases,
    code_watch_tables: [usize; 2],
    watched: Label,
    unwatched: Label,
) {
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(
        e,
        r15_tables,
        TABLE_SLOT_PHYSICAL_PAGES,
        map.physical_pages(),
        Reg::RDX,
    );
    e.load_r32_sib_scale4(Reg::RCX, Reg::RDX, Reg::RCX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    e.store_r64_disp8(Reg::RSP, STACK_WATCH_PAGE, Reg::RCX);
    let second = e.label();
    emit_code_watch_table_branch(
        e,
        width,
        r15_tables,
        TABLE_SLOT_CODE_WATCH_STICKY,
        code_watch_tables[0],
        watched,
        second,
    );
    e.place(second);
    emit_code_watch_table_branch(
        e,
        width,
        r15_tables,
        TABLE_SLOT_CODE_WATCH_NATIVE,
        code_watch_tables[1],
        watched,
        unwatched,
    );
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn emit_code_watch_table_branch(
    e: &mut Encoder,
    width: MemoryWidth,
    r15_tables: bool,
    table_slot: usize,
    code_watch_table: usize,
    watched: Label,
    unwatched: Label,
) {
    e.load_r64_disp8(Reg::RCX, Reg::RSP, STACK_WATCH_PAGE);
    emit_table_base(e, r15_tables, table_slot, code_watch_table, Reg::RDX);
    e.load_r64_sib_scale8(Reg::RDX, Reg::RDX, Reg::RCX);
    e.cmp_r64_imm32(Reg::RDX, 0);
    e.jz(unwatched);

    // Test every granule the access spans, in a sequence whose SIZE does not grow with the width.
    //
    // The guard used to emit one `bt` probe per granule, which was correct but grew linearly:
    // `FSTP TBYTE` at one-byte granularity became ten probes, and the full Doom loop emitted 4039
    // bytes against a one-host-page install limit. That is not a test failure, it is blocks
    // silently REFUSED for size on the oracles. An access at page offset `o` spanning `n` granules
    // occupies bit range `[g, g + n)` of the page's mask, where `g = o >> CHUNK_SHIFT`; instead of
    // probing each bit, load a window over the mask and test a shifted constant against it. One
    // load, one dynamic shift, one AND, for EVERY width.
    //
    // `n` is computed here, at emit time, from the width alone, and it is the granule span an
    // access of this width can reach at ANY offset: an N-byte access starting anywhere inside a
    // granule touches at most `ceil((N + G - 1) / G)` of them, which is the expression below.
    //
    // Written as a worst case rather than as the ALIGNED span because the alignment test is no
    // longer emitted at every call site -- the lean one-lookup load and store sites serve
    // misaligned page-local accesses natively -- so the count must not depend on an alignment
    // nobody promises. At `CHUNK_SHIFT == 0` (the shipped granule) `GRANULE_MASK` is zero and this
    // is identical to the aligned formula, so the repair emits byte-identical code today; its only
    // evidence is a mutation that raises the shift and stores at an ODD offset.
    //
    // Erring high is safe in one direction only, and that is the direction taken: a larger `n` can
    // report a MISS on a granule the access does not touch -- a false WATCHED, i.e. a spurious side
    // exit -- never a missed invalidation. `code_watch.rs` const-asserts that the widest access's
    // worst-case span still fits the 32-bit window this guard tests.
    const GRANULE_MASK: u32 = (1 << NATIVE_CHUNK_SHIFT) - 1;
    // The window bound `code_watch.rs` const-asserts is stated against a WIDEST access, and that
    // number lives there because it is checked in a const context that cannot name this enum. Pin
    // the two together here, where both are in scope, so a wider `MemoryWidth` fails the build
    // rather than silently outgrowing the window it is checked against.
    const _: () = assert!(
        MemoryWidth::Tbyte.bytes() as usize
            == crate::jit::code_watch::NATIVE_WIDEST_GUARDED_ACCESS_BYTES,
        "code_watch's window bound must be stated against the widest guarded access"
    );
    // `bytes() - 1` is the access's EXTENT here, the offset of its last byte, which is what
    // decides how many granules the access spans. Not an alignment mask and not the split charge,
    // both of which share the spelling and neither of which would be correct.
    let n = ((width.bytes() - 1 + GRANULE_MASK) >> NATIVE_CHUNK_SHIFT) + 1;

    if n == 1 {
        // One granule, one probe: the window sequence below is bigger than a single `bt`, and byte
        // stores dominate Doom's inner loop, so the whole change only SHRINKS emitted code if this
        // case keeps the cheap form. At `CHUNK_SHIFT == 0` the granule index IS the page offset,
        // so the shift is omitted entirely rather than emitted as `shr ecx, 0`.
        //
        // Under the worst-case `n` above, `n == 1` requires `width.bytes() == 1` at EVERY shift --
        // a Byte access, which cannot straddle a granule at any offset. So the single-`bt` form
        // stays exactly right once alignment is no longer promised; it is not reachable by a wide
        // access that merely happens to be aligned.
        e.mov_r32_r32(Reg::RCX, Reg::RAX);
        e.and_r32_imm32(Reg::RCX, 0x0fff);
        if NATIVE_CHUNK_SHIFT != 0 {
            e.shift_r32_imm8(5, Reg::RCX, NATIVE_CHUNK_SHIFT as u8);
        }
        e.bt_r64_mem(Reg::RDX, Reg::RCX);
        e.jcc(2, watched);
        e.jmp(unwatched);
        return;
    }

    // The multi-granule window. RCX and RDX ONLY, and that constraint is load-bearing: the natural
    // third scratch would be RSI, and RSI is never free here. Integer blocks do not spill it (it
    // is callee-saved and corrupting the Rust caller is UB) and float blocks keep the live x87
    // status/tag pack in it, so the `FSTP TBYTE` path that motivates this design would corrupt FPU
    // state on every guarded store. `r` is recomputable from RAX, so two registers suffice. RAX
    // and RDI both survive; RDI is live across the guard in the read-modify-write and x87 paths.
    // RDX is reloaded from the table at the top of every call, so clobbering it with the window is
    // safe even though the second table's branch runs after the first's.
    //
    // The window is 32 bits, not 64. The highest real bit tested is `r + n - 1`, and `r <= 7` with
    // `n <= 10` (Tbyte at one-byte granules), so `r + n - 1 <= 16 < 32`. A 32-bit load also
    // shrinks the overhang past the last real mask byte to three bytes, which `ChunkMask`'s single
    // pad word covers.
    //
    // The result is bit-exact against the probe loop it replaces, neither more nor less
    // conservative: `(1 << n) - 1` shifted by `r` has set bits at exactly the positions of
    // granules `g..g+n`, and every one of those bits is inside the loaded window because
    // `b - (g & !7) = r + (b - g) <= 7 + 9 = 16 < 32`.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.and_r32_imm32(Reg::RCX, 0x0fff);
    e.shift_r32_imm8(5, Reg::RCX, (3 + NATIVE_CHUNK_SHIFT) as u8);
    // `mov edx, [rdx + rcx*1]` -- the window, unaligned, over the mask RDX points at.
    e.load_r32_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    // `r`, the first granule's bit position inside the window. The page mask is not needed: the
    // final `and 7` keeps only bits below 3 + CHUNK_SHIFT, which is at most 5 and so well inside
    // the page offset's twelve bits.
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    if NATIVE_CHUNK_SHIFT != 0 {
        e.shift_r32_imm8(5, Reg::RCX, NATIVE_CHUNK_SHIFT as u8);
    }
    e.and_r32_imm32(Reg::RCX, 7);
    e.shift_r32_cl(5, Reg::RDX);
    e.and_r32_imm32(Reg::RDX, (1u32 << n) - 1);
    e.jnz(watched);
    e.jmp(unwatched);
}
