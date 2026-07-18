// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Track C C1b: the CLIF unit compiler for the 13 register/immediate `DirectKind` variants
//! (design sections 2-4). A unit executes its LEADING run of lowerable slots natively
//! (register/immediate forms, plus x87 slots as call-outs through the section 1 ABI) and
//! side-exits at the first non-lowered slot (memory forms, terminals, or anything else),
//! which the interpreter then retires, exactly like a C1a shell boundary.
//!
//! State model: all eight GPR homes, the four `PendingFlags` fields, and `registers.eflags`
//! are loaded into SSA variables at unit entry and spilled at every exit point (a unit that
//! never touched a value stores back the identical bytes, so the load-all/spill-all shape is
//! byte-transparent). Flag reads (ADC/SBB carry-in, INC/DEC's CF preservation, the shift
//! fallbacks) replicate the interpreter's `arith_flag` decision tree over the SSA pending
//! descriptor at RUNTIME (branch-free selects), so an entry-state descriptor of unknown
//! shape needs no compile-time provenance tracking; cranelift folds the tree when the
//! producing instruction is in the same unit and its tag is a known constant.
//!
//! Flag oracles, copied bit-for-bit (design section 3.1/M3): `arith_flag` (core.rs:900),
//! `alu_add`/`alu_add_eager`/`alu_sub`/`alu_sub_eager` (core.rs:1184-1250), `alu_logic`
//! (core.rs:1252), `inc_dec` (core.rs:1158), `shift_rotate` + `set_shift_result_flags`
//! (core.rs:1063/1440) with `jit_set_shift_flags_shr` (core.rs:1540) as the verified
//! replica, and `double_shift` (core.rs:1003). Guest immediates load from the unit's
//! immediate table (F4); the slot OFFSET is structural and baked, the VALUE never is.

use core::mem::offset_of;

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{
    Block, Function, InstBuilder, MemFlagsData, UserFuncName, Value, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use super::super::direct::{
    DirectAddr, DirectKind, MAX_BLOCK_INSTRUCTIONS, MemoryWidth, SegmentLayout, ShiftCount,
    SideExitReason, StoreSource,
};
use super::super::fast_map::{
    NATIVE_KIND_MASK, NATIVE_PAGE_SHIFT, NATIVE_PAGE_USER, NATIVE_PAGE_WRITABLE, NATIVE_RAM_KIND,
    NATIVE_UNAVAILABLE_BIAS, NativeMapBases,
};
use super::ClifBackend;
use super::cache::{ClifAccessCounts, UnitLayout};
use super::callout::{
    CLIF_CALLOUT_CONTINUE, ClifCallOutTable, callout_shim_signature, callout_unit_signature,
};
use crate::{CpuGsw, PendingFlags, Registers};

/// The six arithmetic EFLAGS bits (CF|PF|AF|ZF|SF|OF).
const ARITH_MASK: u32 = 0x8d5;

/// The memory side-exit disposition family (C1c, design section 3's "genuinely new
/// wrinkle", resolved per the question ledger's distinct-code lean): low byte 4 (disjoint
/// from `CLIF_CALLOUT_CONTINUE`/`EXIT`/`HARD_STOP` at 0/1/2), the failing slot's index in
/// bits 16..24, and the diagnostic `SideExitReason` in bits 8..16. The compiled unit itself
/// returns the failing slot, so `run_clif_unit`'s prefix charging treats a memory exit and a
/// call-out exit uniformly once the slot is known. The reason lane is diagnostic only: every
/// memory check side-exits at the un-advanced EIP with zero guest-visible state change, so
/// the guest cannot distinguish which check fired (design section 2's fidelity note).
pub(crate) const CLIF_MEM_EXIT: i64 = 4;

pub(crate) fn clif_mem_exit_disposition(slot: usize, reason: SideExitReason) -> i64 {
    CLIF_MEM_EXIT | ((reason as i64) << 8) | ((slot as i64) << 16)
}

pub(crate) fn clif_mem_exit_slot(disposition: i64) -> usize {
    ((disposition >> 16) & 0xff) as usize
}

pub(crate) fn clif_mem_exit_reason(disposition: i64) -> u32 {
    ((disposition >> 8) & 0xff) as u32
}

/// Compile-time-baked memory context for one unit (the clif analogue of Direct's
/// `MemoryEmitContext`): the FastMap structure-of-arrays bases, the two code-watch table
/// bases (index 0 the decode cache's sticky watch, index 1 the shared hoisted
/// `NativeCodeWatch`), the captured segment snapshot, and the CPL3 flag. All four are
/// STRUCTURAL compile-time data (table addresses, segment layout, privilege), not guest
/// operand values, so baking them as constants is exempt from F4 exactly as Direct's own
/// `mov_r64_imm64` bakes are.
#[derive(Clone, Copy)]
pub(crate) struct UnitMemoryContext {
    pub(crate) map: Option<NativeMapBases>,
    pub(crate) code_watch_tables: [usize; 2],
    pub(crate) segments: SegmentLayout,
    pub(crate) cpl3: bool,
}

/// The static execution plan derived from a walked unit's kinds: which leading slots run
/// natively and the cumulative charge profile (x87 slots excluded per the no-double-charge
/// invariant, design section 5).
pub(crate) struct UnitPlan {
    pub(crate) leading: u8,
    pub(crate) x87_mask: u32,
    pub(crate) cum_raw_before: [u16; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) cum_lowered_before: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) raw_clocks_total: u16,
    pub(crate) lowered_total: u8,
    /// Cumulative per-width access counts before each slot plus the full-run totals (C1c,
    /// design section 4/M4), through `DirectKind`'s own accessors verbatim.
    pub(crate) cum_access_before: [ClifAccessCounts; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) access_total: ClifAccessCounts,
}

fn lowerable(kind: &DirectKind) -> bool {
    // C1c increment 1 adds Load and Store (the two single-access memory forms); the
    // remaining seven memory variants stay growth-run stoppers until their increments land.
    matches!(
        kind,
        DirectKind::MovReg { .. }
            | DirectKind::MovRegByte { .. }
            | DirectKind::MovImm { .. }
            | DirectKind::MovImmByte { .. }
            | DirectKind::Lea { .. }
            | DirectKind::IncDecReg { .. }
            | DirectKind::AluReg { .. }
            | DirectKind::AluImm { .. }
            | DirectKind::AluByteImm { .. }
            | DirectKind::Test { .. }
            | DirectKind::TestImmReg { .. }
            | DirectKind::Shift { .. }
            | DirectKind::DoubleShiftReg { .. }
            | DirectKind::Load { .. }
            | DirectKind::Store { .. }
            | DirectKind::Push { .. }
            | DirectKind::Pop { .. }
    )
}

/// Compute the leading native run and the static charge profile (Direct's own per-kind cost
/// table, `DirectKind::raw_clocks`, reused verbatim; x87 slots excluded from both raw
/// clocks and the lowered/fetch population, design section 5's field split).
pub(crate) fn plan_unit(kinds: &[DirectKind], admit_x87: bool) -> UnitPlan {
    let mut plan = UnitPlan {
        leading: 0,
        x87_mask: 0,
        cum_raw_before: [0; MAX_BLOCK_INSTRUCTIONS],
        cum_lowered_before: [0; MAX_BLOCK_INSTRUCTIONS],
        raw_clocks_total: 0,
        lowered_total: 0,
        cum_access_before: [ClifAccessCounts::default(); MAX_BLOCK_INSTRUCTIONS],
        access_total: ClifAccessCounts::default(),
    };
    let mut raw = 0u16;
    let mut lowered = 0u8;
    let mut access = ClifAccessCounts::default();
    for (i, kind) in kinds.iter().enumerate() {
        plan.cum_raw_before[i] = raw;
        plan.cum_lowered_before[i] = lowered;
        plan.cum_access_before[i] = access;
        if matches!(kind, DirectKind::X87 { .. }) && admit_x87 {
            plan.x87_mask |= 1 << i;
            plan.leading = (i + 1) as u8;
            continue;
        }
        if !lowerable(kind) {
            break;
        }
        raw = raw.saturating_add(kind.raw_clocks() as u16);
        lowered += 1;
        // M4: the per-width counts come from DirectKind's own accessors VERBATIM, never a
        // re-derivation, so the two backends' static counts are equal by shared code.
        access.byte_reads += kind.byte_reads();
        access.word_reads += kind.word_reads();
        access.dword_reads += kind.dword_reads();
        access.byte_stores += kind.byte_stores();
        access.word_stores += kind.word_stores();
        access.dword_stores += kind.dword_stores();
        plan.leading = (i + 1) as u8;
    }
    plan.raw_clocks_total = raw;
    plan.lowered_total = lowered;
    plan.access_total = access;
    plan
}

struct Vars {
    gpr: [Variable; 8],
    tag: Variable,
    pa: Variable,
    pb: Variable,
    pres: Variable,
    eflags: Variable,
}

struct Offsets {
    gpr: i32,
    eip: i32,
    eflags: i32,
    pf: i32,
}

fn offsets() -> Offsets {
    let regs = offset_of!(CpuGsw, registers);
    Offsets {
        gpr: i32::try_from(regs + offset_of!(Registers, gpr)).expect("gpr offset fits"),
        eip: i32::try_from(regs + offset_of!(Registers, eip)).expect("eip offset fits"),
        eflags: i32::try_from(regs + offset_of!(Registers, eflags)).expect("eflags offset fits"),
        pf: i32::try_from(offset_of!(CpuGsw, pending_flags)).expect("pending offset fits"),
    }
}

struct Ctx {
    cpu: Value,
    bus: Value,
    table: Value,
    imm_table: Value,
}

fn width_mask(width: MemoryWidth) -> u32 {
    match width {
        MemoryWidth::Byte => 0xff,
        MemoryWidth::Word => 0xffff,
        MemoryWidth::Dword => 0xffff_ffff,
    }
}

fn width_sign(width: MemoryWidth) -> u32 {
    match width {
        MemoryWidth::Byte => 0x80,
        MemoryWidth::Word => 0x8000,
        MemoryWidth::Dword => 0x8000_0000,
    }
}

fn width_tag(width: MemoryWidth) -> u32 {
    match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 0x100,
        MemoryWidth::Dword => 0x200,
    }
}

/// One compiled-unit build. Straight-line except the intra-instruction diamonds (ADC/SBB's
/// carry branch, the shifts' zero-count skip, the x87 disposition check); `Variable`s carry
/// the guest state across every join.
struct UnitBuilder<'a> {
    b: FunctionBuilder<'a>,
    vars: Vars,
    offs: Offsets,
    ctx: Ctx,
    mem: UnitMemoryContext,
    entry_eip: u32,
    shim_sig: cranelift_codegen::ir::SigRef,
}

impl<'a> UnitBuilder<'a> {
    fn flags(&self) -> MemFlagsData {
        MemFlagsData::trusted()
    }

    fn iconst(&mut self, v: u32) -> Value {
        self.b.ins().iconst(types::I32, i64::from(v))
    }

    fn gpr32(&mut self, i: u8) -> Value {
        self.b.use_var(self.vars.gpr[i as usize])
    }

    fn set_gpr32(&mut self, i: u8, v: Value) {
        self.b.def_var(self.vars.gpr[i as usize], v);
    }

    fn gpr16(&mut self, i: u8) -> Value {
        let v = self.gpr32(i);
        self.b.ins().band_imm(v, 0xffff)
    }

    fn set_gpr16(&mut self, i: u8, v: Value) {
        let old = self.gpr32(i);
        let high = self.b.ins().band_imm(old, 0xffff_0000u32 as i64);
        let low = self.b.ins().band_imm(v, 0xffff);
        let merged = self.b.ins().bor(high, low);
        self.set_gpr32(i, merged);
    }

    /// Byte-lane read per the x86 sub-register convention: indices 0-3 the low byte of
    /// gpr[0..3], indices 4-7 bits 8-15 of gpr[0..3].
    fn gpr8(&mut self, i: u8) -> Value {
        let (reg, shift) = if i < 4 { (i, 0) } else { (i - 4, 8) };
        let v = self.gpr32(reg);
        let v = if shift != 0 {
            self.b.ins().ushr_imm(v, shift)
        } else {
            v
        };
        self.b.ins().band_imm(v, 0xff)
    }

    fn set_gpr8(&mut self, i: u8, v: Value) {
        let (reg, shift) = if i < 4 { (i, 0) } else { (i - 4, 8) };
        let lane_mask = !(0xffu32 << shift);
        let old = self.gpr32(reg);
        let kept = self.b.ins().band_imm(old, i64::from(lane_mask));
        let v = self.b.ins().band_imm(v, 0xff);
        let v = if shift != 0 {
            self.b.ins().ishl_imm(v, shift)
        } else {
            v
        };
        let merged = self.b.ins().bor(kept, v);
        self.set_gpr32(reg, merged);
    }

    fn read_width(&mut self, i: u8, width: MemoryWidth) -> Value {
        match width {
            MemoryWidth::Byte => self.gpr8(i),
            MemoryWidth::Word => self.gpr16(i),
            MemoryWidth::Dword => self.gpr32(i),
        }
    }

    fn write_width(&mut self, i: u8, width: MemoryWidth, v: Value) {
        match width {
            MemoryWidth::Byte => self.set_gpr8(i, v),
            MemoryWidth::Word => self.set_gpr16(i, v),
            MemoryWidth::Dword => self.set_gpr32(i, v),
        }
    }

    /// The operand table holds two u32 lanes per slot (immediate at `slot * 8`,
    /// displacement at `slot * 8 + 4`); the offsets are structural and baked, the values
    /// never are (F4).
    fn imm32(&mut self, slot: usize) -> Value {
        let off = i32::try_from(slot * 8).expect("imm slot offset fits");
        let flags = self.flags();
        self.b
            .ins()
            .load(types::I32, flags, self.ctx.imm_table, off)
    }

    fn imm8(&mut self, slot: usize) -> Value {
        let off = i32::try_from(slot * 8).expect("imm slot offset fits");
        let flags = self.flags();
        let v = self.b.ins().load(types::I8, flags, self.ctx.imm_table, off);
        self.b.ins().uextend(types::I32, v)
    }

    /// The addressing-mode displacement lane (design section 1.2's F4 extension).
    fn disp32(&mut self, slot: usize) -> Value {
        let off = i32::try_from(slot * 8 + 4).expect("disp slot offset fits");
        let flags = self.flags();
        self.b
            .ins()
            .load(types::I32, flags, self.ctx.imm_table, off)
    }

    fn set_pending(&mut self, tag: Value, a: Value, b: Value, result: Value) {
        self.b.def_var(self.vars.tag, tag);
        self.b.def_var(self.vars.pa, a);
        self.b.def_var(self.vars.pb, b);
        self.b.def_var(self.vars.pres, result);
    }

    fn bit(&mut self, v: Value, bit: u8) -> Value {
        let v = if bit != 0 {
            self.b.ins().ushr_imm(v, i64::from(bit))
        } else {
            v
        };
        self.b.ins().band_imm(v, 1)
    }

    /// i32 0/1: is the SSA pending descriptor live (tag bit 31)?
    fn pending_live(&mut self) -> Value {
        let tag = self.b.use_var(self.vars.tag);
        self.bit(tag, 31)
    }

    /// The runtime width mask/sign of the SSA pending descriptor (from the tag's width
    /// byte), replicating `PendingFlags::width` for the entry-state case; folds to a
    /// constant when the producer is in-unit.
    fn pending_mask_sign(&mut self) -> (Value, Value) {
        let tag = self.b.use_var(self.vars.tag);
        let w = self.b.ins().ushr_imm(tag, 8);
        let w = self.b.ins().band_imm(w, 0xff);
        let is_byte = self.b.ins().icmp_imm(IntCC::Equal, w, 0);
        let is_word = self.b.ins().icmp_imm(IntCC::Equal, w, 1);
        let mask_byte = self.iconst(0xff);
        let mask_word = self.iconst(0xffff);
        let mask_dword = self.iconst(0xffff_ffff);
        let mask_tail = self.b.ins().select(is_word, mask_word, mask_dword);
        let mask = self.b.ins().select(is_byte, mask_byte, mask_tail);
        let sign_byte = self.iconst(0x80);
        let sign_word = self.iconst(0x8000);
        let sign_dword = self.iconst(0x8000_0000);
        let sign_tail = self.b.ins().select(is_word, sign_word, sign_dword);
        let sign = self.b.ins().select(is_byte, sign_byte, sign_tail);
        (mask, sign)
    }

    /// i32 0/1 for one of CF/AF/OF, replicating `CpuGsw::flag`/`arith_flag` (core.rs:900)
    /// over the SSA pending descriptor and eflags variable at runtime.
    fn flag_cf(&mut self) -> Value {
        let live = self.pending_live();
        let tag = self.b.use_var(self.vars.tag);
        let a = self.b.use_var(self.vars.pa);
        let bv = self.b.use_var(self.vars.pb);
        let eflags = self.b.use_var(self.vars.eflags);
        let (mask, _) = self.pending_mask_sign();
        let op = self.b.ins().band_imm(tag, 0xff);
        let is_logic = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, op, 2);
        let is_sub = self.b.ins().icmp_imm(IntCC::Equal, op, 1);
        let has_ov = self.bit(tag, 16);
        let ov_val = self.bit(tag, 17);
        // Sub: a < b (both stored width-masked). Add: a + b > mask (64-bit compare).
        let cf_sub = self.b.ins().icmp(IntCC::UnsignedLessThan, a, bv);
        let cf_sub = self.b.ins().uextend(types::I32, cf_sub);
        let a64 = self.b.ins().uextend(types::I64, a);
        let b64 = self.b.ins().uextend(types::I64, bv);
        let sum = self.b.ins().iadd(a64, b64);
        let mask64 = self.b.ins().uextend(types::I64, mask);
        let cf_add = self.b.ins().icmp(IntCC::UnsignedGreaterThan, sum, mask64);
        let cf_add = self.b.ins().uextend(types::I32, cf_add);
        let zero = self.iconst(0);
        let cf_arith = self.b.ins().select(is_sub, cf_sub, cf_add);
        let cf_base = self.b.ins().select(is_logic, zero, cf_arith);
        let cf_pending = self.b.ins().select(has_ov, ov_val, cf_base);
        let cf_live = self.bit(eflags, 0);
        self.b.ins().select(live, cf_pending, cf_live)
    }

    fn flag_af(&mut self) -> Value {
        let live = self.pending_live();
        let tag = self.b.use_var(self.vars.tag);
        let a = self.b.use_var(self.vars.pa);
        let bv = self.b.use_var(self.vars.pb);
        let result = self.b.use_var(self.vars.pres);
        let eflags = self.b.use_var(self.vars.eflags);
        let op = self.b.ins().band_imm(tag, 0xff);
        let is_logic = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, op, 2);
        let x = self.b.ins().bxor(a, bv);
        let x = self.b.ins().bxor(x, result);
        let af_arith = self.bit(x, 4);
        let af_live = self.bit(eflags, 4);
        let af_pending = self.b.ins().select(is_logic, af_live, af_arith);
        self.b.ins().select(live, af_pending, af_live)
    }

    fn flag_of(&mut self) -> Value {
        let live = self.pending_live();
        let tag = self.b.use_var(self.vars.tag);
        let a = self.b.use_var(self.vars.pa);
        let bv = self.b.use_var(self.vars.pb);
        let result = self.b.use_var(self.vars.pres);
        let eflags = self.b.use_var(self.vars.eflags);
        let (_, sign) = self.pending_mask_sign();
        let op = self.b.ins().band_imm(tag, 0xff);
        let is_logic = self
            .b
            .ins()
            .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, op, 2);
        let is_sub = self.b.ins().icmp_imm(IntCC::Equal, op, 1);
        // Sub: (a^b) & (a^result) & sign. Add: (a^result) & (b^result) & sign.
        let axb = self.b.ins().bxor(a, bv);
        let axr = self.b.ins().bxor(a, result);
        let bxr = self.b.ins().bxor(bv, result);
        let sub_bits = self.b.ins().band(axb, axr);
        let sub_bits = self.b.ins().band(sub_bits, sign);
        let of_sub = self.b.ins().icmp_imm(IntCC::NotEqual, sub_bits, 0);
        let of_sub = self.b.ins().uextend(types::I32, of_sub);
        let add_bits = self.b.ins().band(axr, bxr);
        let add_bits = self.b.ins().band(add_bits, sign);
        let of_add = self.b.ins().icmp_imm(IntCC::NotEqual, add_bits, 0);
        let of_add = self.b.ins().uextend(types::I32, of_add);
        let zero = self.iconst(0);
        let of_arith = self.b.ins().select(is_sub, of_sub, of_add);
        let of_pending = self.b.ins().select(is_logic, zero, of_arith);
        let of_live = self.bit(eflags, 11);
        self.b.ins().select(live, of_pending, of_live)
    }

    /// ZF/SF/PF bits (i32 0/1) of a width-masked result.
    fn szp_bits(&mut self, result: Value, sign: u32) -> (Value, Value, Value) {
        let zf = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let zf = self.b.ins().uextend(types::I32, zf);
        let sf_bits = self.b.ins().band_imm(result, i64::from(sign));
        let sf = self.b.ins().icmp_imm(IntCC::NotEqual, sf_bits, 0);
        let sf = self.b.ins().uextend(types::I32, sf);
        let low = self.b.ins().band_imm(result, 0xff);
        let ones = self.b.ins().popcnt(low);
        let odd = self.b.ins().band_imm(ones, 1);
        let pf = self.b.ins().bxor_imm(odd, 1);
        (zf, sf, pf)
    }

    /// Write all six arithmetic bits live (0/1-valued inputs), OR the reserved bit 0x2, and
    /// clear the SSA pending descriptor (the eager-write shape shared by ADC/SBB's carry
    /// arm; the interpreter's net effect through `set_flag`/`set_szp` from any prior
    /// pending state).
    fn write_eflags_all(
        &mut self,
        cf: Value,
        pf: Value,
        af: Value,
        zf: Value,
        sf: Value,
        of: Value,
    ) {
        let eflags = self.b.use_var(self.vars.eflags);
        let kept = self
            .b
            .ins()
            .band_imm(eflags, !(i64::from(ARITH_MASK)) & 0xffff_ffff);
        let pf = self.b.ins().ishl_imm(pf, 2);
        let af = self.b.ins().ishl_imm(af, 4);
        let zf = self.b.ins().ishl_imm(zf, 6);
        let sf = self.b.ins().ishl_imm(sf, 7);
        let of = self.b.ins().ishl_imm(of, 11);
        let mut bits = self.b.ins().bor(cf, pf);
        bits = self.b.ins().bor(bits, af);
        bits = self.b.ins().bor(bits, zf);
        bits = self.b.ins().bor(bits, sf);
        bits = self.b.ins().bor(bits, of);
        let merged = self.b.ins().bor(kept, bits);
        let merged = self.b.ins().bor_imm(merged, 0x2);
        self.b.def_var(self.vars.eflags, merged);
        let zero = self.iconst(0);
        self.set_pending(zero, zero, zero, zero);
    }

    /// Spill every SSA-resident piece of guest state back to `CpuGsw`, with EIP set to
    /// `entry_eip + delta` (the materialize-before-exit discipline, design section 4).
    fn spill_all(&mut self, eip_delta: u32) {
        let flags = self.flags();
        for i in 0..8u8 {
            let v = self.gpr32(i);
            self.b
                .ins()
                .store(flags, v, self.ctx.cpu, self.offs.gpr + i32::from(i) * 4);
        }
        let tag = self.b.use_var(self.vars.tag);
        let a = self.b.use_var(self.vars.pa);
        let bv = self.b.use_var(self.vars.pb);
        let result = self.b.use_var(self.vars.pres);
        self.b.ins().store(flags, tag, self.ctx.cpu, self.offs.pf);
        self.b.ins().store(flags, a, self.ctx.cpu, self.offs.pf + 4);
        self.b
            .ins()
            .store(flags, bv, self.ctx.cpu, self.offs.pf + 8);
        self.b
            .ins()
            .store(flags, result, self.ctx.cpu, self.offs.pf + 12);
        let eflags = self.b.use_var(self.vars.eflags);
        self.b
            .ins()
            .store(flags, eflags, self.ctx.cpu, self.offs.eflags);
        let eip = self.iconst(self.entry_eip.wrapping_add(eip_delta));
        self.b.ins().store(flags, eip, self.ctx.cpu, self.offs.eip);
    }

    /// Reload every variable from `CpuGsw` (the call-out-return Continue reload, design
    /// section 4: the shim may have mutated any GPR-adjacent state through the interpreter,
    /// so the defensive all-state reload is used, per the design's recommended first cut).
    fn reload_all(&mut self) {
        let flags = self.flags();
        for i in 0..8usize {
            let v = self.b.ins().load(
                types::I32,
                flags,
                self.ctx.cpu,
                self.offs.gpr + (i as i32) * 4,
            );
            self.b.def_var(self.vars.gpr[i], v);
        }
        let tag = self
            .b
            .ins()
            .load(types::I32, flags, self.ctx.cpu, self.offs.pf);
        let a = self
            .b
            .ins()
            .load(types::I32, flags, self.ctx.cpu, self.offs.pf + 4);
        let bv = self
            .b
            .ins()
            .load(types::I32, flags, self.ctx.cpu, self.offs.pf + 8);
        let result = self
            .b
            .ins()
            .load(types::I32, flags, self.ctx.cpu, self.offs.pf + 12);
        self.set_pending(tag, a, bv, result);
        let eflags = self
            .b
            .ins()
            .load(types::I32, flags, self.ctx.cpu, self.offs.eflags);
        self.b.def_var(self.vars.eflags, eflags);
    }

    /// Lower one slot. `delta` is the guest-byte offset of this slot from unit entry.
    fn lower_slot(&mut self, kind: &DirectKind, slot: usize, delta: u32, len: u8) {
        match *kind {
            DirectKind::MovReg { dst, src, width } => {
                let v = self.read_width(src, width);
                self.write_width(dst, width, v);
            }
            DirectKind::MovRegByte { dst, src } => {
                let v = self.gpr8(src);
                self.set_gpr8(dst, v);
            }
            DirectKind::MovImm { dst, .. } => {
                let v = self.imm32(slot);
                self.set_gpr32(dst, v);
            }
            DirectKind::MovImmByte { dst, .. } => {
                let v = self.imm8(slot);
                self.set_gpr8(dst, v);
            }
            DirectKind::Lea { dst, addr } => {
                // Effective-address arithmetic only (never dereferenced); the displacement
                // is a guest immediate and loads from the table (F4), the register/scale
                // shape is structural.
                let mut v = self.imm32(slot);
                if let Some(base) = addr.base {
                    let b = self.gpr32(base);
                    v = self.b.ins().iadd(v, b);
                }
                if let Some(index) = addr.index {
                    let idx = self.gpr32(index);
                    let scaled = match addr.scale {
                        1 => idx,
                        2 => self.b.ins().ishl_imm(idx, 1),
                        4 => self.b.ins().ishl_imm(idx, 2),
                        _ => self.b.ins().ishl_imm(idx, 3),
                    };
                    v = self.b.ins().iadd(v, scaled);
                }
                self.set_gpr32(dst, v);
            }
            DirectKind::IncDecReg { dst, is_dec, width } => {
                self.lower_inc_dec(dst, is_dec, width);
            }
            DirectKind::AluReg {
                op,
                dst,
                src,
                width,
            } => {
                let b = self.read_width(src, width);
                self.lower_alu(op, dst, b, width);
            }
            DirectKind::AluImm { op, dst, .. } => {
                let b = self.imm32(slot);
                self.lower_alu(op, dst, b, MemoryWidth::Dword);
            }
            DirectKind::AluByteImm { op, dst, .. } => {
                let b = self.imm8(slot);
                self.lower_alu(op, dst, b, MemoryWidth::Byte);
            }
            DirectKind::Test { a, b } => {
                let va = self.gpr32(a);
                let vb = self.gpr32(b);
                let result = self.b.ins().band(va, vb);
                self.lower_logic_flags(result, MemoryWidth::Dword);
            }
            DirectKind::TestImmReg { dst, width, .. } => {
                let va = self.read_width(dst, width);
                let vb = match width {
                    MemoryWidth::Byte => self.imm8(slot),
                    _ => self.imm32(slot),
                };
                let vb = self.b.ins().band_imm(vb, i64::from(width_mask(width)));
                let result = self.b.ins().band(va, vb);
                self.lower_logic_flags(result, width);
            }
            DirectKind::Shift { op, dst, .. } => {
                self.lower_shift(op, dst, slot);
            }
            DirectKind::DoubleShiftReg {
                left,
                dst,
                src,
                count,
            } => {
                self.lower_double_shift(left, dst, src, count, slot);
            }
            DirectKind::Load {
                dst, width, addr, ..
            } => {
                self.lower_load(dst, width, &addr, slot, delta);
            }
            DirectKind::Store {
                source,
                width,
                addr,
                ..
            } => {
                self.lower_store(source, width, &addr, slot, delta);
            }
            DirectKind::Push { source } => {
                self.lower_push(source, slot, delta);
            }
            DirectKind::Pop { dst } => {
                self.lower_pop(dst, slot, delta);
            }
            DirectKind::X87 { .. } => {
                self.lower_x87_callout(slot, delta, len);
            }
            _ => unreachable!("non-lowerable kind reached the unit body"),
        }
    }

    fn iconst64(&mut self, v: u64) -> Value {
        self.b.ins().iconst(types::I64, v as i64)
    }

    /// One failing-check branch: on `cond`, jump to the slot's shared side-exit block
    /// carrying the encoded disposition (slot index plus the diagnostic reason); otherwise
    /// continue in a fresh sealed block.
    fn check_fail(&mut self, cond: Value, side: Block, slot: usize, reason: SideExitReason) {
        let disp = self
            .b
            .ins()
            .iconst(types::I64, clif_mem_exit_disposition(slot, reason));
        let ok = self.b.create_block();
        self.b.ins().brif(cond, side, &[disp.into()], ok, &[]);
        self.b.switch_to_block(ok);
        self.b.seal_block(ok);
    }

    /// Effective address for a memory form: displacement from the operand table's
    /// displacement lane (F4, design section 1.2), register/scale shape structural,
    /// mirroring `emit_effective_address` (emit.rs:1725).
    fn mem_effective_address(&mut self, addr: &DirectAddr, slot: usize) -> Value {
        let mut v = self.disp32(slot);
        if let Some(base) = addr.base {
            let b = self.gpr32(base);
            v = self.b.ins().iadd(v, b);
        }
        if let Some(index) = addr.index {
            let idx = self.gpr32(index);
            let scaled = match addr.scale {
                1 => idx,
                2 => self.b.ins().ishl_imm(idx, 1),
                4 => self.b.ins().ishl_imm(idx, 2),
                _ => self.b.ins().ishl_imm(idx, 3),
            };
            v = self.b.ins().iadd(v, scaled);
        }
        v
    }

    /// Steps 1-4 of the normative check sequence (design section 2's intro): segment limit
    /// (including the compile-time underflow edge), the wide-page guard, the identity/kind
    /// check, and the CPL3-conditional permission check. Returns `(linear, page64)`: the
    /// segmented linear address and the linear page index widened to i64, with the
    /// permission step already applied. The clippy arity allowance is deliberate: this is
    /// the one shared body of the per-access check sequence and every argument is a
    /// distinct check input; splitting it would scatter the normative order across
    /// helpers.
    #[allow(clippy::too_many_arguments)]
    fn mem_checked_page(
        &mut self,
        map: NativeMapBases,
        segment: crate::SegmentIndex,
        eff: Value,
        width: MemoryWidth,
        slot: usize,
        side: Block,
        write: bool,
    ) -> (Value, Value) {
        // Step 1: segment limit against the EFFECTIVE address, before the base add,
        // matching emit_segmented_linear_address (emit.rs:1741). The descriptor is the
        // unit's own captured snapshot, compile-time data (the entry guards revalidate it
        // against live segment state before every entry).
        let descriptor = self.mem.segments.descriptor(segment);
        if descriptor.limit != u32::MAX {
            match descriptor.limit.checked_sub(width.bytes() - 1) {
                None => {
                    // m3's underflow edge: no access of this width fits under the limit,
                    // so the check is an unconditional side exit (emitted as a
                    // constant-true branch so the block structure stays uniform).
                    let t = self.iconst(1);
                    self.check_fail(t, side, slot, SideExitReason::Other);
                }
                Some(max_start) => {
                    let cond = self.b.ins().icmp_imm(
                        IntCC::UnsignedGreaterThan,
                        eff,
                        i64::from(max_start),
                    );
                    self.check_fail(cond, side, slot, SideExitReason::Other);
                }
            }
        }
        let linear = if descriptor.base != 0 {
            self.b.ins().iadd_imm(eff, i64::from(descriptor.base))
        } else {
            eff
        };
        // Step 2: wide-page guard, reject-only (design section 2.6): misalignment, then the
        // page-boundary cross (`> 0x1000 - width`, not `>=`). Byte accesses skip both.
        if width.needs_alignment_guard() {
            let off = self.b.ins().band_imm(linear, i64::from(width.bytes() - 1));
            let cond = self.b.ins().icmp_imm(IntCC::NotEqual, off, 0);
            self.check_fail(cond, side, slot, SideExitReason::CrossPageOrAlignment);
            let poff = self.b.ins().band_imm(linear, 0xfff);
            let cond = self.b.ins().icmp_imm(
                IntCC::UnsignedGreaterThan,
                poff,
                i64::from(0x1000 - width.bytes()),
            );
            self.check_fail(cond, side, slot, SideExitReason::CrossPageOrAlignment);
        }
        // Step 3: identity/kind. INCREMENT-1 RESTRICTION (recorded deviation): only
        // NATIVE_RAM_KIND continues; a mode13 page side-exits to the interpreter's
        // canonical access instead of taking Direct's mode13 arm, because the dynamic
        // mode13 counter/dirty lanes are not threaded through the clif unit yet. Sound and
        // byte-identical (the failing slot charges nothing natively and the interpreter
        // re-executes it), merely slower on VGA-touching units until the mode13 increment.
        let page = self.b.ins().ushr_imm(linear, i64::from(NATIVE_PAGE_SHIFT));
        let page64 = self.b.ins().uextend(types::I64, page);
        let flags_base = self.iconst64(map.flags() as u64);
        let flags_addr = self.b.ins().iadd(flags_base, page64);
        let memflags = self.flags();
        let fb = self.b.ins().uload8(types::I32, memflags, flags_addr, 0);
        let kind = self.b.ins().band_imm(fb, i64::from(NATIVE_KIND_MASK));
        let cond = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, kind, i64::from(NATIVE_RAM_KIND));
        self.check_fail(cond, side, slot, SideExitReason::UnavailableOrKind);
        // Step 4: permission, CPL3-conditional at COMPILE time (two code shapes, never a
        // runtime CPL read), matching emit_read/write_permission_check (emit.rs:2047-2072):
        // ring 0 emits no check at all (a populated bias already proves the walk admitted
        // the current context while CR0.WP is clear).
        if self.mem.cpl3 {
            let need = if write {
                i64::from(NATIVE_PAGE_USER | NATIVE_PAGE_WRITABLE)
            } else {
                i64::from(NATIVE_PAGE_USER)
            };
            let bits = self.b.ins().band_imm(fb, need);
            let cond = self.b.ins().icmp_imm(IntCC::NotEqual, bits, need);
            self.check_fail(cond, side, slot, SideExitReason::Permission);
        }
        (linear, page64)
    }

    /// One bias-table lookup plus the UNAVAILABLE sentinel check (design section 2.3, the
    /// pointer-producing step and the epoch mechanism's actual enforcement point), then the
    /// host pointer `bias + linear`.
    fn mem_host_pointer(
        &mut self,
        biases_base: usize,
        page64: Value,
        linear: Value,
        slot: usize,
        side: Block,
    ) -> Value {
        let idx8 = self.b.ins().ishl_imm(page64, 3);
        let base = self.iconst64(biases_base as u64);
        let addr = self.b.ins().iadd(base, idx8);
        let memflags = self.flags();
        let bias = self.b.ins().load(types::I64, memflags, addr, 0);
        let cond = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, bias, NATIVE_UNAVAILABLE_BIAS as i64);
        self.check_fail(cond, side, slot, SideExitReason::UnavailableOrKind);
        let lin64 = self.b.ins().uextend(types::I64, linear);
        self.b.ins().iadd(bias, lin64)
    }

    /// Both code-watch families by inline bit-test (design section 2.5, mirroring
    /// emit_code_watch_branch, emit.rs:2134-2185): the physical page number indexes each
    /// baked table; a null page pointer skips; a set chunk bit side-exits. A wide access
    /// additionally tests the last byte's chunk (same-page by the wide guard, possibly a
    /// different 16-byte chunk).
    fn code_watch_checks(
        &mut self,
        map: NativeMapBases,
        linear: Value,
        page64: Value,
        width: MemoryWidth,
        slot: usize,
        side: Block,
    ) {
        let memflags = self.flags();
        let idx4 = self.b.ins().ishl_imm(page64, 2);
        let ppb = self.iconst64(map.physical_pages() as u64);
        let pa = self.b.ins().iadd(ppb, idx4);
        let phys = self.b.ins().load(types::I32, memflags, pa, 0);
        let wp = self.b.ins().ushr_imm(phys, i64::from(NATIVE_PAGE_SHIFT));
        let wp64 = self.b.ins().uextend(types::I64, wp);
        for table in self.mem.code_watch_tables {
            let t = self.iconst64(table as u64);
            let idx8 = self.b.ins().ishl_imm(wp64, 3);
            let ta = self.b.ins().iadd(t, idx8);
            let ptr = self.b.ins().load(types::I64, memflags, ta, 0);
            let has = self.b.ins().icmp_imm(IntCC::NotEqual, ptr, 0);
            let test_blk = self.b.create_block();
            let next_blk = self.b.create_block();
            self.b.ins().brif(has, test_blk, &[], next_blk, &[]);
            self.b.switch_to_block(test_blk);
            self.b.seal_block(test_blk);
            let last = width.bytes() - 1;
            let offsets: &[u32] = if width.needs_alignment_guard() {
                &[0, 1]
            } else {
                &[0]
            };
            for &which in offsets {
                let po = self.b.ins().band_imm(linear, 0xfff);
                let po = if which != 0 {
                    self.b.ins().iadd_imm(po, i64::from(last))
                } else {
                    po
                };
                let chunk = self.b.ins().ushr_imm(po, 4);
                let word_idx = self.b.ins().ushr_imm(chunk, 6);
                let wi64 = self.b.ins().uextend(types::I64, word_idx);
                let wi8 = self.b.ins().ishl_imm(wi64, 3);
                let wa = self.b.ins().iadd(ptr, wi8);
                let word = self.b.ins().load(types::I64, memflags, wa, 0);
                let bitidx = self.b.ins().band_imm(chunk, 63);
                let bit64 = self.b.ins().uextend(types::I64, bitidx);
                let shifted = self.b.ins().ushr(word, bit64);
                let bit = self.b.ins().band_imm(shifted, 1);
                let cond = self.b.ins().icmp_imm(IntCC::NotEqual, bit, 0);
                self.check_fail(cond, side, slot, SideExitReason::CodeWatch);
            }
            self.b.ins().jump(next_blk, &[]);
            self.b.switch_to_block(next_blk);
            self.b.seal_block(next_blk);
        }
    }

    /// Terminate one memory slot: jump the happy path to a fresh block, then fill the
    /// slot's shared side-exit block (materialize state at the UN-ADVANCED failing slot's
    /// EIP per design section 3, return the encoded disposition).
    fn finish_mem_slot(&mut self, side: Block, delta: u32) {
        let done = self.b.create_block();
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(side);
        self.b.seal_block(side);
        let disp = self.b.block_params(side)[0];
        self.spill_all(delta);
        self.b.ins().return_(&[disp]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }

    /// `Load` (design section 1.3 item 1): the READ check list, one notrap load through the
    /// read bias, the partial-write-aware destination write. No flags.
    fn lower_load(
        &mut self,
        dst: u8,
        width: MemoryWidth,
        addr: &DirectAddr,
        slot: usize,
        delta: u32,
    ) {
        let map = self
            .mem
            .map
            .expect("memory-form slot requires fast-map bases");
        let side = self.b.create_block();
        self.b.append_block_param(side, types::I64);
        let eff = self.mem_effective_address(addr, slot);
        let (linear, page64) =
            self.mem_checked_page(map, addr.segment, eff, width, slot, side, false);
        let host = self.mem_host_pointer(map.read_biases(), page64, linear, slot, side);
        let memflags = self.flags();
        let value = match width {
            MemoryWidth::Byte => self.b.ins().uload8(types::I32, memflags, host, 0),
            MemoryWidth::Word => self.b.ins().uload16(types::I32, memflags, host, 0),
            MemoryWidth::Dword => self.b.ins().load(types::I32, memflags, host, 0),
        };
        self.write_width(dst, width, value);
        self.finish_mem_slot(side, delta);
    }

    /// `Store` (design section 1.3 item 2): the WRITE check list, the code-watch check
    /// (both families) BEFORE the store commits, then one notrap store through the write
    /// bias. No flags. Nothing is committed before the last check passes (section 2.9's
    /// discipline is trivial here: the store instruction is the only effect).
    fn lower_store(
        &mut self,
        source: StoreSource,
        width: MemoryWidth,
        addr: &DirectAddr,
        slot: usize,
        delta: u32,
    ) {
        let map = self
            .mem
            .map
            .expect("memory-form slot requires fast-map bases");
        let side = self.b.create_block();
        self.b.append_block_param(side, types::I64);
        let eff = self.mem_effective_address(addr, slot);
        let (linear, page64) =
            self.mem_checked_page(map, addr.segment, eff, width, slot, side, true);
        let host = self.mem_host_pointer(map.write_biases(), page64, linear, slot, side);
        self.code_watch_checks(map, linear, page64, width, slot, side);
        let value = match source {
            StoreSource::Reg(src) => self.read_width(src, width),
            StoreSource::Imm(_) => {
                let v = self.imm32(slot);
                self.b.ins().band_imm(v, i64::from(width_mask(width)))
            }
            StoreSource::EipDelta(_) => {
                unreachable!("EipDelta store sources exist only inside Call, a terminal")
            }
        };
        let memflags = self.flags();
        match width {
            MemoryWidth::Byte => {
                self.b.ins().istore8(memflags, value, host, 0);
            }
            MemoryWidth::Word => {
                self.b.ins().istore16(memflags, value, host, 0);
            }
            MemoryWidth::Dword => {
                self.b.ins().store(memflags, value, host, 0);
            }
        }
        self.finish_mem_slot(side, delta);
    }

    /// `Push` (design section 1.3 item 8): `Store` at `[ESP - 4]`, dword, through the SS
    /// descriptor (the SS-base discipline of `stack_addr`, emit.rs:6-14), then `ESP -= 4`
    /// as a plain SSA subtract AFTER the store commits. Store-before-decrement makes
    /// `PUSH ESP` store the PRE-decrement value, the architecturally correct one, with no
    /// special case; and every side exit leaves ESP unmodified by SSA construction (the
    /// side block's predecessors all branch before the ESP redefinition). The implicit
    /// displacement -4 is STRUCTURAL (no guest byte encodes it, so no re-stamp can ever
    /// patch it), baked per the same F4 exemption 0xd1's implicit count uses; the pushed
    /// immediate (0x68/0x6a), by contrast, is a guest operand and loads from the table.
    /// Only ever compiled when `cpu.stack_is_32bit()` (the walker gate, section 1.4), so
    /// no 16-bit SP wrap logic exists here, mirroring Direct's omission exactly.
    fn lower_push(&mut self, source: StoreSource, slot: usize, delta: u32) {
        let map = self
            .mem
            .map
            .expect("memory-form slot requires fast-map bases");
        let side = self.b.create_block();
        self.b.append_block_param(side, types::I64);
        let esp = self.gpr32(4);
        let eff = self.b.ins().iadd_imm(esp, -4);
        let (linear, page64) = self.mem_checked_page(
            map,
            crate::SegmentIndex::Ss,
            eff,
            MemoryWidth::Dword,
            slot,
            side,
            true,
        );
        let host = self.mem_host_pointer(map.write_biases(), page64, linear, slot, side);
        self.code_watch_checks(map, linear, page64, MemoryWidth::Dword, slot, side);
        let value = match source {
            StoreSource::Reg(src) => self.gpr32(src),
            StoreSource::Imm(_) => self.imm32(slot),
            StoreSource::EipDelta(_) => {
                unreachable!("EipDelta store sources exist only inside Call, a terminal")
            }
        };
        let memflags = self.flags();
        self.b.ins().store(memflags, value, host, 0);
        let new_esp = self.b.ins().iadd_imm(esp, -4);
        self.set_gpr32(4, new_esp);
        self.finish_mem_slot(side, delta);
    }

    /// `Pop` (design section 1.3 item 9), in Direct's exact order (emit.rs:431-434): run
    /// `Load`'s READ check list at `[ESP]` (dword, SS) into a scratch SSA value, THEN
    /// `ESP += 4`, THEN write the scratch into `dst`'s home. The increment-before-dst-write
    /// order is load-bearing: for `POP ESP` the final dst write overwrites the incremented
    /// ESP with the LOADED value, which is what x86 requires; a write-then-increment order
    /// would compute loaded + 4. The implicit displacement 0 is structural, like Push's -4.
    fn lower_pop(&mut self, dst: u8, slot: usize, delta: u32) {
        let map = self
            .mem
            .map
            .expect("memory-form slot requires fast-map bases");
        let side = self.b.create_block();
        self.b.append_block_param(side, types::I64);
        let esp = self.gpr32(4);
        let (linear, page64) = self.mem_checked_page(
            map,
            crate::SegmentIndex::Ss,
            esp,
            MemoryWidth::Dword,
            slot,
            side,
            false,
        );
        let host = self.mem_host_pointer(map.read_biases(), page64, linear, slot, side);
        let memflags = self.flags();
        let value = self.b.ins().load(types::I32, memflags, host, 0);
        let new_esp = self.b.ins().iadd_imm(esp, 4);
        self.set_gpr32(4, new_esp);
        self.set_gpr32(dst, value);
        self.finish_mem_slot(side, delta);
    }

    /// `inc_dec` (core.rs:1158): Add/Sub pending tag with the CURRENT CF preserved through
    /// `cf_override` (tag bit 16 set, bit 17 the value) and `b = 1`, byte-for-byte the
    /// interpreter's `PendingFlags::from_legacy` output.
    fn lower_inc_dec(&mut self, dst: u8, is_dec: bool, width: MemoryWidth) {
        let mask = width_mask(width);
        let a = self.read_width(dst, width);
        let result = if is_dec {
            self.b.ins().iadd_imm(a, -1)
        } else {
            self.b.ins().iadd_imm(a, 1)
        };
        let result = self.b.ins().band_imm(result, i64::from(mask));
        let cf = self.flag_cf();
        let base_tag = 0x8001_0000u32 | width_tag(width) | u32::from(is_dec);
        let cf_shifted = self.b.ins().ishl_imm(cf, 17);
        let base = self.iconst(base_tag);
        let tag = self.b.ins().bor(base, cf_shifted);
        let one = self.iconst(1);
        self.set_pending(tag, a, one, result);
        self.write_width(dst, width, result);
    }

    /// The `alu_logic` shape shared by OR/AND/XOR/TEST (core.rs:1252): AF is written LIVE
    /// (the materialized current AF), the reserved bit is set, and the Logic-tag pending
    /// descriptor carries only the result.
    fn lower_logic_flags(&mut self, result: Value, width: MemoryWidth) {
        let af = self.flag_af();
        let eflags = self.b.use_var(self.vars.eflags);
        let kept = self.b.ins().band_imm(eflags, !0x10i64 & 0xffff_ffff);
        let af_shifted = self.b.ins().ishl_imm(af, 4);
        let merged = self.b.ins().bor(kept, af_shifted);
        let merged = self.b.ins().bor_imm(merged, 0x2);
        self.b.def_var(self.vars.eflags, merged);
        let tag = self.iconst(0x8000_0002 | width_tag(width));
        let zero = self.iconst(0);
        self.set_pending(tag, zero, zero, result);
    }

    /// ADD/OR/ADC/SBB/AND/SUB/XOR/CMP over a register destination, `b` already fetched and
    /// width-masked by the caller. Copies `CpuGsw::alu` (core.rs:979) and the lazy/eager
    /// split of `alu_add`/`alu_sub` (core.rs:1184-1250).
    fn lower_alu(&mut self, op: u8, dst: u8, b_in: Value, width: MemoryWidth) {
        let mask = width_mask(width);
        let sign = width_sign(width);
        let a = self.read_width(dst, width);
        let b_masked = self.b.ins().band_imm(b_in, i64::from(mask));
        match op {
            0 | 5 | 7 => {
                // Lazy ADD/SUB/CMP: the two-operand pending descriptor.
                let is_sub = op != 0;
                let result = if is_sub {
                    self.b.ins().isub(a, b_masked)
                } else {
                    self.b.ins().iadd(a, b_masked)
                };
                let result = self.b.ins().band_imm(result, i64::from(mask));
                let tag = self.iconst(0x8000_0000 | width_tag(width) | u32::from(is_sub));
                self.set_pending(tag, a, b_masked, result);
                if op != 7 {
                    self.write_width(dst, width, result);
                }
            }
            1 | 4 | 6 => {
                let result = match op {
                    1 => self.b.ins().bor(a, b_masked),
                    4 => self.b.ins().band(a, b_masked),
                    _ => self.b.ins().bxor(a, b_masked),
                };
                let result = self.b.ins().band_imm(result, i64::from(mask));
                self.lower_logic_flags(result, width);
                self.write_width(dst, width, result);
            }
            2 | 3 => {
                // ADC/SBB: the interpreter's runtime carry branch (design section 3.3).
                // Carry clear behaves exactly like ADD/SUB (lazy descriptor); carry set
                // computes flags eagerly per alu_add_eager/alu_sub_eager.
                let cf = self.flag_cf();
                let carry_block = self.b.create_block();
                let clear_block = self.b.create_block();
                let join = self.b.create_block();
                self.b.ins().brif(cf, carry_block, &[], clear_block, &[]);

                self.b.switch_to_block(clear_block);
                self.b.seal_block(clear_block);
                {
                    let is_sub = op == 3;
                    let result = if is_sub {
                        self.b.ins().isub(a, b_masked)
                    } else {
                        self.b.ins().iadd(a, b_masked)
                    };
                    let result = self.b.ins().band_imm(result, i64::from(mask));
                    let tag = self.iconst(0x8000_0000 | width_tag(width) | u32::from(is_sub));
                    self.set_pending(tag, a, b_masked, result);
                    self.write_width(dst, width, result);
                }
                self.b.ins().jump(join, &[]);

                self.b.switch_to_block(carry_block);
                self.b.seal_block(carry_block);
                {
                    let a64 = self.b.ins().uextend(types::I64, a);
                    let b64 = self.b.ins().uextend(types::I64, b_masked);
                    let (result, cf_new, of) = if op == 2 {
                        // alu_add_eager with carry == 1.
                        let sum = self.b.ins().iadd(a64, b64);
                        let sum = self.b.ins().iadd_imm(sum, 1);
                        let result = self.b.ins().ireduce(types::I32, sum);
                        let result = self.b.ins().band_imm(result, i64::from(mask));
                        let cf_new =
                            self.b
                                .ins()
                                .icmp_imm(IntCC::UnsignedGreaterThan, sum, i64::from(mask));
                        let cf_new = self.b.ins().uextend(types::I32, cf_new);
                        let axr = self.b.ins().bxor(a, result);
                        let bxr = self.b.ins().bxor(b_masked, result);
                        let bits = self.b.ins().band(axr, bxr);
                        let bits = self.b.ins().band_imm(bits, i64::from(sign));
                        let of = self.b.ins().icmp_imm(IntCC::NotEqual, bits, 0);
                        let of = self.b.ins().uextend(types::I32, of);
                        (result, cf_new, of)
                    } else {
                        // alu_sub_eager with borrow == 1: rhs = b + 1, result = a - rhs.
                        let rhs = self.b.ins().iadd_imm(b64, 1);
                        let diff = self.b.ins().isub(a64, rhs);
                        let result = self.b.ins().ireduce(types::I32, diff);
                        let result = self.b.ins().band_imm(result, i64::from(mask));
                        let cf_new = self.b.ins().icmp(IntCC::UnsignedLessThan, a64, rhs);
                        let cf_new = self.b.ins().uextend(types::I32, cf_new);
                        let axb = self.b.ins().bxor(a, b_masked);
                        let axr = self.b.ins().bxor(a, result);
                        let bits = self.b.ins().band(axb, axr);
                        let bits = self.b.ins().band_imm(bits, i64::from(sign));
                        let of = self.b.ins().icmp_imm(IntCC::NotEqual, bits, 0);
                        let of = self.b.ins().uextend(types::I32, of);
                        (result, cf_new, of)
                    };
                    let x = self.b.ins().bxor(a, b_masked);
                    let x = self.b.ins().bxor(x, result);
                    let af = self.bit(x, 4);
                    let (zf, sf, pf) = self.szp_bits(result, sign);
                    self.write_eflags_all(cf_new, pf, af, zf, sf, of);
                    self.write_width(dst, width, result);
                }
                self.b.ins().jump(join, &[]);

                self.b.switch_to_block(join);
                self.b.seal_block(join);
            }
            _ => unreachable!("alu op {op}"),
        }
    }

    /// Single-register shift (SHL/SHR/SAR, dword-only per the classifier), count loaded
    /// from the immediate table. Oracles: `shift_rotate` (core.rs:1063) +
    /// `set_shift_result_flags` (core.rs:1440), with `jit_set_shift_flags_shr`
    /// (core.rs:1540) the verified closed-form replica for SHR.
    fn lower_shift(&mut self, op: u8, dst: u8, slot: usize) {
        let count_raw = self.imm8(slot);
        let count = self.b.ins().band_imm(count_raw, 0x1f);
        let shift_block = self.b.create_block();
        let join = self.b.create_block();
        // A zero masked count is a true no-op: no flag, no write, pending untouched.
        let is_zero = self.b.ins().icmp_imm(IntCC::Equal, count, 0);
        self.b.ins().brif(is_zero, join, &[], shift_block, &[]);

        self.b.switch_to_block(shift_block);
        self.b.seal_block(shift_block);
        {
            let v = self.gpr32(dst);
            let count_m1 = self.b.ins().iadd_imm(count, -1);
            let (result, cf) = match op {
                4 | 6 => {
                    // SHL: result = v << count; CF = bit (32 - count) of the original.
                    let result = self.b.ins().ishl(v, count);
                    let thirty_two = self.iconst(32);
                    let shamt = self.b.ins().isub(thirty_two, count);
                    let cf = self.b.ins().ushr(v, shamt);
                    let cf = self.b.ins().band_imm(cf, 1);
                    (result, cf)
                }
                5 => {
                    let result = self.b.ins().ushr(v, count);
                    let cf = self.b.ins().ushr(v, count_m1);
                    let cf = self.b.ins().band_imm(cf, 1);
                    (result, cf)
                }
                _ => {
                    let result = self.b.ins().sshr(v, count);
                    let cf = self.b.ins().ushr(v, count_m1);
                    let cf = self.b.ins().band_imm(cf, 1);
                    (result, cf)
                }
            };
            // OF is defined only at count == 1 (SHL: MSB(result) ^ CF; SHR: MSB(original);
            // SAR: 0); otherwise it falls back per set_shift_result_flags.
            let of_defined = match op {
                4 | 6 => {
                    let msb = self.bit(result, 31);
                    self.b.ins().bxor(msb, cf)
                }
                5 => self.bit(v, 31),
                _ => self.iconst(0),
            };
            let count_is_one = self.b.ins().icmp_imm(IntCC::Equal, count, 1);
            self.commit_shift_flags(result, cf, of_defined, count_is_one, MemoryWidth::Dword);
            self.set_gpr32(dst, result);
        }
        self.b.ins().jump(join, &[]);

        self.b.switch_to_block(join);
        self.b.seal_block(join);
    }

    /// SHLD/SHRD over a register pair (dword-only per the classifier). Oracle:
    /// `double_shift` (core.rs:1003); the dword form never reaches the count > bits rotate
    /// arm (a 5-bit masked count is at most 31).
    fn lower_double_shift(&mut self, left: bool, dst: u8, src: u8, count: ShiftCount, slot: usize) {
        let count = match count {
            ShiftCount::Immediate(_) => self.imm8(slot),
            ShiftCount::Cl => self.gpr8(1),
        };
        let count = self.b.ins().band_imm(count, 0x1f);
        let shift_block = self.b.create_block();
        let join = self.b.create_block();
        let is_zero = self.b.ins().icmp_imm(IntCC::Equal, count, 0);
        self.b.ins().brif(is_zero, join, &[], shift_block, &[]);

        self.b.switch_to_block(shift_block);
        self.b.seal_block(shift_block);
        {
            let d = self.gpr32(dst);
            let s = self.gpr32(src);
            let thirty_two = self.iconst(32);
            let inv = self.b.ins().isub(thirty_two, count);
            let count_m1 = self.b.ins().iadd_imm(count, -1);
            let (result, cf) = if left {
                // SHLD: dest shifts left, vacated low bits take src's high bits; CF is the
                // last bit shifted out of dest (bit 32 - count of the original dest).
                let hi = self.b.ins().ishl(d, count);
                let lo = self.b.ins().ushr(s, inv);
                let result = self.b.ins().bor(hi, lo);
                let cf = self.b.ins().ushr(d, inv);
                let cf = self.b.ins().band_imm(cf, 1);
                (result, cf)
            } else {
                // SHRD: dest shifts right, vacated high bits take src's low bits; CF is
                // bit count - 1 of the original dest.
                let lo = self.b.ins().ushr(d, count);
                let hi = self.b.ins().ishl(s, inv);
                let result = self.b.ins().bor(lo, hi);
                let cf = self.b.ins().ushr(d, count_m1);
                let cf = self.b.ins().band_imm(cf, 1);
                (result, cf)
            };
            // OF (count == 1 only): the sign bit changed.
            let change = self.b.ins().bxor(d, result);
            let of_defined = self.bit(change, 31);
            let count_is_one = self.b.ins().icmp_imm(IntCC::Equal, count, 1);
            self.commit_shift_flags(result, cf, of_defined, count_is_one, MemoryWidth::Dword);
            self.set_gpr32(dst, result);
        }
        self.b.ins().jump(join, &[]);

        self.b.switch_to_block(join);
        self.b.seal_block(join);
    }

    /// `set_shift_result_flags` (core.rs:1440) for a nonzero count: when a pending
    /// descriptor exists, ALL six bits are written live (undefined ones falling back to the
    /// materialized pending values) and pending clears; when none exists, only
    /// CF/SZP (and OF at count 1) are written and the stale pending BYTES are preserved
    /// untouched (the interpreter leaves them; the descriptor is compared byte-for-byte).
    fn commit_shift_flags(
        &mut self,
        result: Value,
        cf: Value,
        of_defined: Value,
        count_is_one: Value,
        width: MemoryWidth,
    ) {
        let sign = width_sign(width);
        let live = self.pending_live();
        let af_fallback = self.flag_af();
        let of_fallback = self.flag_of();
        let eflags = self.b.use_var(self.vars.eflags);
        let af_current = self.bit(eflags, 4);
        let of_current = self.bit(eflags, 11);
        let af = self.b.ins().select(live, af_fallback, af_current);
        let of_undefined = self.b.ins().select(live, of_fallback, of_current);
        let of = self.b.ins().select(count_is_one, of_defined, of_undefined);
        let (zf, sf, pf) = self.szp_bits(result, sign);
        // write_eflags_all clears pending unconditionally; preserve the stale bytes when no
        // descriptor was live (the interpreter's pending-none arm never touches them).
        let old_tag = self.b.use_var(self.vars.tag);
        let old_a = self.b.use_var(self.vars.pa);
        let old_b = self.b.use_var(self.vars.pb);
        let old_res = self.b.use_var(self.vars.pres);
        self.write_eflags_all(cf, pf, af, zf, sf, of);
        let zero = self.iconst(0);
        let tag = self.b.ins().select(live, zero, old_tag);
        let a = self.b.ins().select(live, zero, old_a);
        let bv = self.b.ins().select(live, zero, old_b);
        let res = self.b.ins().select(live, zero, old_res);
        self.set_pending(tag, a, bv, res);
    }

    /// The x87 call-out slot (design sections 1 and 4): materialize everything, call the
    /// shim through the table, and either continue (reloading all state defensively) or
    /// return the shim's disposition unchanged.
    fn lower_x87_callout(&mut self, _slot: usize, delta: u32, len: u8) {
        self.spill_all(delta);
        let flags = self.flags();
        let shim = self.b.ins().load(
            types::I64,
            flags,
            self.ctx.table,
            i32::try_from(core::mem::offset_of!(ClifCallOutTable, x87))
                .expect("table slot offset fits"),
        );
        let site_eip = self
            .b
            .ins()
            .iconst(types::I32, i64::from(self.entry_eip.wrapping_add(delta)));
        let fetch_len = self.b.ins().iconst(types::I32, i64::from(len));
        let call = self.b.ins().call_indirect(
            self.shim_sig,
            shim,
            &[self.ctx.cpu, self.ctx.bus, site_eip, fetch_len],
        );
        let disposition = self.b.inst_results(call)[0];
        let cont = self.b.create_block();
        let exit = self.b.create_block();
        let is_continue = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, disposition, CLIF_CALLOUT_CONTINUE);
        self.b
            .ins()
            .brif(is_continue, cont, &[], exit, &[disposition.into()]);

        self.b.append_block_param(exit, types::I64);
        self.b.switch_to_block(exit);
        self.b.seal_block(exit);
        let exit_disposition = self.b.block_params(exit)[0];
        self.b.ins().return_(&[exit_disposition]);

        self.b.switch_to_block(cont);
        self.b.seal_block(cont);
        self.reload_all();
    }
}

/// Compile one unit's leading run. Returns the installed entry address; `None` falls back
/// to the interpreter (arena full, relocation reject, unsupported host).
pub(crate) fn compile_unit(
    backend: &mut ClifBackend,
    layout: &UnitLayout,
    plan: &UnitPlan,
    entry_eip: u32,
    mem: UnitMemoryContext,
) -> Option<usize> {
    debug_assert!(plan.leading >= 1);
    let mut func =
        Function::with_name_signature(UserFuncName::user(0, 50), callout_unit_signature());
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let shim_sig =
        builder.import_signature(callout_shim_signature(backend.isa().default_call_conv()));

    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let bus = builder.block_params(entry)[1];
    let table = builder.block_params(entry)[2];
    let imm_table = builder.block_params(entry)[3];

    let vars = Vars {
        gpr: core::array::from_fn(|_| builder.declare_var(types::I32)),
        tag: builder.declare_var(types::I32),
        pa: builder.declare_var(types::I32),
        pb: builder.declare_var(types::I32),
        pres: builder.declare_var(types::I32),
        eflags: builder.declare_var(types::I32),
    };

    let mut ub = UnitBuilder {
        b: builder,
        vars,
        offs: offsets(),
        ctx: Ctx {
            cpu,
            bus,
            table,
            imm_table,
        },
        mem,
        entry_eip,
        shim_sig,
    };

    // Load-all at entry: byte-transparent (an untouched value spills back identically).
    debug_assert_eq!(offset_of!(PendingFlags, tag), 0);
    ub.reload_all();

    let mut delta = 0u32;
    for slot in 0..plan.leading as usize {
        let kind = &layout.kinds[slot];
        let len = layout.fetch_lens[slot];
        ub.lower_slot(kind, slot, delta, len);
        delta = delta.wrapping_add(u32::from(len));
    }

    // The stop-slot side exit: materialize with EIP at the first non-lowered slot (or the
    // unit end when the whole walked span lowered).
    ub.spill_all(delta);
    let side_exit = ub.b.ins().iconst(types::I64, 0);
    ub.b.ins().return_(&[side_exit]);

    ub.b.finalize();
    backend.finalize(func).map(|ptr| ptr as usize)
}
