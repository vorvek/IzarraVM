// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The one-lookup load path (`dev_docs/2026-08-07-one-lookup-load-design.md`): the lean read
//! site (D3a) replacing `emit_ram_read_pointer`'s classic pair for its ten callers, the parking
//! probe (D3b) for the deferred-completion callers (the Ret/Ret16/JmpMem trio, and DivMem since
//! the FPU-loop-rows slice), the x87 read-pointer probe (D5), and the shared
//! read-resolve stub pad (D4) — the store pad's sibling: six counting stubs (GPR width x cpl,
//! the only width dependence being the mode13 lane they move), two park-only trio stubs and
//! two x87 pack stubs, ten against the store pad's seventeen because reads have no code-watch
//! guard.
//!
//! The counter identity this file exists to preserve (design §2, the run.rs subtraction): RAM
//! reads are counted STATICALLY at compile, only mode13 reads move dynamic lanes — and a
//! MISALIGNED RAM access deposits its extra byte cycles, which is a CLOCK quantity in
//! `STACK_DWORD_READS`'s high half and not a read count, so the static identity survives it: one
//! access still counts once. So the lean
//! fast RAM arm touches NO counter and NO frame slot; every other lean case goes to a
//! width-specific COUNTING stub that moves exactly the width's read lane on a mode13 success,
//! after every status this access can refuse with (an emission-shape correction the L8 size
//! swap forced: the first-cut inline mode13 arm plus a cold per-site completion made every
//! read site ~40 bytes LARGER than the classic front, and native mode13 READS are stub-cold by
//! evidence — Mode X produces no read fills, review F3). The parking probe (trio)
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
/// with a trailing completion — the fast RAM arm writes no frame slot, and the counting stub
/// already moved any mode13 lane). Contract unchanged from the classic pair: RDI = host
/// pointer, RAX preserved, RCX/RDX clobbered, every side exit resolved inside.
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
    // HOISTED above the guard, because the alignment half below targets `slow`. `done` moves with
    // it so the two stay adjacent. Both are still placed on every path: when
    // `emit_segmented_linear_address` takes its `checked_sub` None arm it emits an unconditional
    // jump and returns from ITSELF, not from here, so the rest of this function still emits (as
    // dead code) and still places both labels.
    let slow = e.label();
    let done = e.label();
    emit_segmented_linear_address(e, addr, width, memory, sides, wrap);
    if width.needs_alignment_guard() {
        // The two halves are called DIRECTLY, not through `emit_wide_page_guard`: that wrapper
        // exists for the eleven sites that send both verdicts to one label, and this site does
        // not. A page-CROSSING access still refuses -- the crossing bound is the only thing left
        // keeping a served access inside the one page its entry was resolved against, which is
        // why it is emitted first -- while a MISALIGNED page-local access now falls into this
        // site's own slow stub and is served there.
        emit_page_cross_bound(e, width, sides.cross_page_or_alignment);
        // RDX is the scratch here, as it was before the decomposition: this precedes
        // `emit_load_bias_probe`, whose contract is "RDX is untouched", and nothing downstream of
        // it reads RDX. The STORE site cannot use RDX and uses RCX instead; see
        // `emit_alignment_test`.
        emit_alignment_test(e, width, Reg::RDX, slow);
    }
    emit_load_bias_probe(e, map);

    // Jumping past the probe into `slow` is legal, and neither fact is obvious:
    // `emit_counting_read_stub` never reads incoming RDI -- it recomputes the page index from RAX
    // and re-derives the kind from the flags byte -- and nothing in the stub writes RAX, so its
    // own `test al, mask` sees the linear address this site formed.

    // BOTH privilege arms test both tag bits: a tagged entry's low bits are part of the VALUE,
    // so even a site allowed to read through it (cpl0 through a supervisor entry) must strip
    // them before forming the pointer — the store slice's round-one miscompile class.
    e.test_r8_low_imm8(
        Reg::RDI,
        (NATIVE_LOAD_BIAS_MODE13 | NATIVE_LOAD_BIAS_SUPERVISOR) as u8,
    );
    if memory.cpl3 {
        // At cpl3 EVERY tagged case — poison, supervisor (may not read), mode13 — goes to the
        // counting stub, so the site has no aux arm at all. The stub classifies, permission-
        // checks, resolves, and moves the mode13 lane itself on a mode13 success.
        e.jnz(slow);
        e.add_r64_r64(Reg::RDI, Reg::RAX);
        e.jmp(done);
    } else {
        let fast_join = e.label();
        // At cpl0 the one native tagged case is supervisor RAM — today's checkless ring-0 read,
        // and the COMMON case under a flat-model extender (design F1) — which strips and
        // rejoins. Poison and mode13 go to the counting stub: native mode13 READS exist only
        // under chained 13h (Mode X produces no read fills, review F3), so unlike the store
        // side's doom-hot aperture writes they are stub-cold by evidence, and the inline arm
        // the first cut carried made every read site ~40 bytes LARGER than the classic front
        // (the L8 size swap caught it).
        let aux = e.label();
        e.jnz(aux);
        e.place(fast_join);
        e.add_r64_r64(Reg::RDI, Reg::RAX);
        // NO counter and NO frame-slot write: RAM reads are static (design §2). The caller's
        // load through RDI follows immediately and cannot fault.
        e.jmp(done);
        e.place(aux);
        e.cmp_r64_imm32(Reg::RDI, u32::MAX);
        e.jz(slow);
        e.test_r8_low_imm8(Reg::RDI, NATIVE_LOAD_BIAS_MODE13 as u8);
        e.jnz(slow);
        e.and_r64_imm32(Reg::RDI, (NATIVE_LOAD_BIAS_TAG_MASK as u32) ^ u32::MAX);
        e.jmp(fast_join);
    }

    e.place(slow);
    e.call_m64_disp32(
        Reg::R15,
        table_slot_offset(read_stub_slot_counting(
            gpr_read_width_index(width),
            memory.cpl3,
        )),
    );
    emit_read_status_dispatch(e, memory.cpl3, sides, done);
    e.place(done);
}

/// GPR read width index into the counting-stub slot layout: byte 0, word 1, dword 2.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn gpr_read_width_index(width: MemoryWidth) -> usize {
    match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 1,
        MemoryWidth::Dword => 2,
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR memory reads are never 8- or 10-byte wide")
        }
    }
}

/// The D3b parking probe for the deferred-completion callers (Ret/Ret16/JmpMem, and DivMem
/// since the FPU-loop-rows slice), emitted by
/// `emit_ram_read_pointer_inner` in place of its classic front (after the segmented address and
/// the wide guard). Parks the page KIND in `STACK_READ_KIND` and moves NO counter: each
/// caller's own `emit_mode13_read_completion` call runs after its side exits (the trio's
/// CS-limit check; DivMem's divide guards), preserving the deferred-increment ordering those
/// sites were built around.
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
    // slot. RDX is dead at every caller site (the trio reloads it from RDI after the
    // completion; DivMem reloads it from home(2) before the divide).
    // Hoisting this park above `fast_join` is the F1 miscompile: the chain cell in the battery
    // reads a 4-clock phantom video charge on a supervisor RET when it happens.
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
    e.call_m64_disp32(
        Reg::R15,
        table_slot_offset(read_stub_slot_park(memory.cpl3)),
    );
    // The PARK-ONLY stub: it parked the kind on success and moved no lane — the trio's own
    // completion runs after their limit check, on every arm equally.
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
/// (`read_stub_slot_counting`, then `read_stub_slot_park`, then `read_stub_slot_x87`).
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
    // The deposit gate inside these stubs encodes `alignment_mask()` while the mode13 refusal
    // encodes `bytes() - 1`. For the three widths this pad builds the two are equal, so the two
    // spellings emit the same immediate and the split is a naming change with no byte behind it.
    //
    // Asserted at compile time rather than pinned as a golden byte hash: these stubs never appear
    // in an emitted block, so no block comparison can see them, and a hash would only record what
    // the current build happens to produce. This states the property the equality depends on, and
    // it fails to COMPILE if a future width array or dial change breaks it.
    const _: () = assert!(
        MemoryWidth::Byte.alignment_mask() == MemoryWidth::Byte.bytes() - 1
            && MemoryWidth::Word.alignment_mask() == MemoryWidth::Word.bytes() - 1
            && MemoryWidth::Dword.alignment_mask() == MemoryWidth::Dword.bytes() - 1,
        "the read stub pad's two mask spellings must agree at every width the pad builds"
    );
    let mut code = Vec::new();
    let mut offsets = [0usize; READ_STUB_COUNT];
    let mut cursor = 0usize;
    let widths = [MemoryWidth::Byte, MemoryWidth::Word, MemoryWidth::Dword];
    for width in widths {
        for cpl3 in [false, true] {
            append_stub(
                &mut code,
                &mut offsets,
                &mut cursor,
                read_stub_slot_counting(gpr_read_width_index(width), cpl3) - TABLE_SLOT_READ_STUBS,
                emit_counting_read_stub(width, cpl3, map),
            );
        }
    }
    for cpl3 in [false, true] {
        append_stub(
            &mut code,
            &mut offsets,
            &mut cursor,
            read_stub_slot_park(cpl3) - TABLE_SLOT_READ_STUBS,
            emit_park_read_stub(cpl3, map),
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

/// The counting read stub for the LEAN sites: classify, permission (cpl3 variant only),
/// read-bias resolve — and on a mode13 success, move the width's dynamic read lane ITSELF
/// (the pop prologue put RSP at the frame level, so the increment helpers emit at their normal
/// displacements). No park: the lean sites read no kind afterward, and keeping the frame slot
/// untouched is what the R4 shape rule promises. The kind split happens BEFORE the resolve so
/// the classified kind never needs to survive the page-index recompute — each kind gets its
/// own resolve tail, and stub bytes are per-cache, not per-site.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_counting_read_stub(width: MemoryWidth, cpl3: bool, map: NativeMapBases) -> Vec<u8> {
    let mut e = Encoder::new();
    emit_stub_prologue(&mut e);
    let ram = e.label();
    let status_unavailable = e.label();
    let status_permission = e.label();

    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_table_base(&mut e, true, TABLE_SLOT_FLAGS, map.flags(), Reg::RDX);
    e.movzx_r32_byte_sib(Reg::RDX, Reg::RDX, Reg::RCX);
    e.mov_r32_r32(Reg::RDI, Reg::RDX);
    e.and_r32_imm32(Reg::RDI, u32::from(NATIVE_KIND_MASK));
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_RAM_KIND));
    e.jz(ram);
    e.cmp_r32_imm32(Reg::RDI, u32::from(NATIVE_MODE13_KIND));
    e.jnz(status_unavailable);

    // Fall-through: the mode13 resolve tail.
    //
    // Mode 13h keeps REFUSING misaligned accesses. Conservative rather than proven inequivalent:
    // the interpreter splits an aperture access into bytes exactly as it splits RAM, but nothing
    // has shown the VGA latch and plane state per access is equivalent under a split, and the
    // population does not justify finding out -- wolf3d's entire aperture is 0.01% of its slow
    // reads, and only the misaligned subset reaches here.
    //
    // Placed BEFORE `emit_read_permission_check`, matching where the pre-slice
    // `emit_wide_page_guard` sat relative to every check: refusing later would re-attribute a
    // cpl3 aperture case from alignment to Permission.
    //
    // The refusal returns status 1, so it lands on the site's `unavailable_or_kind` exit and is
    // counted as `UnavailableOrKind`, not `CrossPageOrAlignment`. Deliberate: a new status would
    // add a compare and a branch to EVERY read site's cold dispatch, roughly ten per block, which
    // is emitted-size pressure on the exact constraint the watch-window redesign was built
    // around. The mis-attribution is counters-only and bounded at 0.01%.
    //
    // Wrapped, because `emit_read_stub_pad` builds a stub for every GPR width INCLUDING Byte, and
    // an unwrapped test would emit `test al, 0` plus an untakeable branch into both Byte stubs.
    if width.needs_alignment_guard() {
        // RAX, not RDI: RDI is still the raw probed entry here, and after the resolve below it
        // would be a HOST pointer whose low bits carry the FastMap bias.
        //
        // `bytes() - 1`, NOT `alignment_mask()`, and the difference is deliberate rather than
        // historical. This is a REFUSAL: a nonzero result defers the access to the interpreter.
        // The question it asks is "is this one natural transaction of its full width", which the
        // aperture can serve, and only `bytes() - 1` asks that. The deposit gate further down
        // this pad asks the guard's question instead and reads `alignment_mask()`.
        //
        // The direction matters if a wide width ever routes to a lean site. A SMALLER mask
        // refuses FEWER accesses, so `alignment_mask()` here would ADMIT every Qword address
        // congruent to 4 mod 8 that is refused today. Widening the pad means revisiting this
        // gate, not switching its spelling.
        e.test_r8_low_imm8(Reg::RAX, (width.bytes() - 1) as u8);
        e.jnz(status_unavailable);
    }
    emit_read_permission_check(&mut e, cpl3, status_permission);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_read_pointer(&mut e, true, map, status_unavailable);
    match width {
        MemoryWidth::Byte => emit_dynamic_increment(&mut e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Word => emit_dynamic_word_increment(&mut e, STACK_MODE13_BYTE_READS),
        MemoryWidth::Dword => emit_dynamic_increment(&mut e, STACK_MODE13_DWORD_READS),
        MemoryWidth::Qword | MemoryWidth::Tbyte => {
            unreachable!("GPR memory reads are never 8- or 10-byte wide")
        }
    }
    e.xor_r64_self(Reg::RCX);
    emit_stub_return(&mut e);

    e.place(ram);
    emit_read_permission_check(&mut e, cpl3, status_permission);
    e.mov_r32_r32(Reg::RCX, Reg::RAX);
    e.shift_r32_imm8(5, Reg::RCX, NATIVE_PAGE_SHIFT as u8);
    emit_read_pointer(&mut e, true, map, status_unavailable);
    // The misaligned RAM read this stub now SERVES rather than refuses: charge the extra byte
    // cycles it owes beyond the one wide cycle the block's static count already carries.
    //
    // CONDITIONAL, and that is not an optimisation: an ALIGNED access really can reach this tail
    // and must charge exactly what it charged before the slice.
    //
    // The route is narrow and worth naming precisely, because the two obvious candidates are both
    // WRONG and stating them would make this comment argue against itself. A supervisor-tagged
    // page read at cpl0 strips its tag and rejoins the fast arm INLINE, never entering this stub;
    // a page with no committed read bias hits `emit_read_pointer`'s `UNAVAILABLE_BIAS` arm and
    // returns status 1 without ever reaching this point.
    //
    // What does reach here is a page whose HOST BACKING is not 4 KiB-aligned. `derive_load_bias`
    // poisons the LOAD bias when `read_bias & PAGE_MASK != 0` -- that bias carries the mode13 and
    // supervisor tags in its low bits, so a bias with low bits of its own cannot be tagged --
    // while `read_biases[index]` stays live, and `FastMap::populate` never requires the pointer to
    // be page-aligned. So the site's probe sees poison, jumps here, and `emit_read_pointer`
    // resolves the untagged read bias perfectly well. Every aligned wide read on such a page would
    // be over-charged, permanently and silently, by an unconditional deposit.
    //
    // The test is on RAX because `emit_read_pointer` ends `add rdi, rax`: RDI is now a HOST
    // pointer whose low bits carry the FastMap bias, while RAX is still the linear address and
    // nothing in this stub writes it. RDX is dead here -- the tail is `xor ecx, ecx` and return.
    //
    // Placed after the resolve, so the access is committed and can no longer refuse: a deposit
    // followed by a status-1 return would charge for an access the interpreter is about to redo.
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
        // place it on the shared return so the encoder's every-label-placed invariant holds.
        e.place(status_permission);
    }
    e.finish()
}

/// The shared classify front of the two PARKING stub families (trio and x87): page index from
/// RAX, flags byte into RDX (kept RAW for the permission check), kind split, unknown kinds to
/// `status_unavailable`, and the classified kind staged in ECX for the caller's park. Falls
/// through with RCX = the kind, RDX = the raw flags byte.
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

/// The trio's PARK-ONLY read stub: classify, park the BARE kind in `STACK_READ_KIND` (the
/// classic front's convention at emit.rs — what `emit_mode13_read_completion` compares),
/// permission (cpl3 variant only), read-bias resolve. NO store, NO value spill (loads carry no
/// value — the store slice's F1 hazard class is structurally absent) and NO counter: the
/// trio's own deferred completion moves the lane after the CS-limit check. Unlike the store
/// x87 stub there is no watch guard writing the aliased `STACK_WATCH_PAGE`, so the park needs
/// no staging slot.
#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn emit_park_read_stub(cpl3: bool, map: NativeMapBases) -> Vec<u8> {
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
