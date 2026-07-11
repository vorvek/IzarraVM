// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Admission and storage for page-local direct-code blocks.

use std::collections::HashMap;

use izarravm_core::CpuPersona;

use super::encoder::{Encoder, Reg};
use super::exec_mem::{EXECUTABLE_ARENA_LEN, ExecutableArena};
use crate::{CpuGsw, DecodeGroup, DecodedInsn, DecodedOperand, OperandSize, Prefixes, Registers};

pub(crate) const MAX_BLOCK_INSTRUCTIONS: usize = 32;
pub(crate) const HOT_LOOKUP_LEN: usize = 65_536;

/// Everything that can change the meaning of bytes at a linear entry point.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BlockKey {
    pub linear: u32,
    pub physical: u32,
    pub mode_key: u32,
}

impl BlockKey {
    pub(crate) const fn new(linear: u32, physical: u32, mode_key: u32) -> Self {
        Self {
            linear,
            physical,
            mode_key,
        }
    }

    fn hot_index(self) -> usize {
        self.linear as usize & (HOT_LOOKUP_LEN - 1)
    }
}

/// Validated guest extent for one compiled block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct BlockSpan {
    pub key: BlockKey,
    pub guest_len: u16,
    pub instructions: u8,
}

impl BlockSpan {
    pub(crate) fn new(key: BlockKey, guest_len: usize, instructions: usize) -> Option<Self> {
        if guest_len == 0 || !(1..=MAX_BLOCK_INSTRUCTIONS).contains(&instructions) {
            return None;
        }
        let guest_len = u16::try_from(guest_len).ok()?;
        let last = u32::from(guest_len) - 1;
        let linear_last = key.linear.checked_add(last)?;
        let physical_last = key.physical.checked_add(last)?;
        if key.linear >> 12 != linear_last >> 12 || key.physical >> 12 != physical_last >> 12 {
            return None;
        }
        Some(Self {
            key,
            guest_len,
            instructions: instructions as u8,
        })
    }
}

/// Metadata for one sealed native block. Its entry remains valid until its cache is cleared.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CompiledBlock {
    span: BlockSpan,
    entry: usize,
    fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    raw_clocks: u16,
}

impl CompiledBlock {
    pub(crate) fn span(&self) -> BlockSpan {
        self.span
    }

    pub(crate) fn entry_ptr(&self) -> *const u8 {
        self.entry as *const u8
    }

    pub(crate) fn fetch_lens(&self) -> &[u8] {
        &self.fetch_lens[..usize::from(self.span.instructions)]
    }

    pub(crate) fn raw_clocks(&self) -> u32 {
        u32::from(self.raw_clocks)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BlockId(u16);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockState {
    Seen,
    Rejected,
    Compiled(BlockId),
}

#[derive(Clone, Copy)]
struct HotEntry {
    key: BlockKey,
    id: BlockId,
    generation: u32,
}

/// Result of observing a block entry. A new key is interpreted once, then becomes eligible for
/// compilation on its next observation.
#[derive(Debug)]
pub(crate) enum BlockProbe {
    Interpret,
    Rejected,
    Compile,
    Ready(CompiledBlock),
}

/// Bounded direct-block cache. Hash lookup is authoritative; the direct-mapped table is only a
/// collision-checked accelerator. Capacity pressure clears the entire cache.
pub(crate) struct BlockCache {
    entries: HashMap<BlockKey, BlockState>,
    blocks: Vec<CompiledBlock>,
    hot: Box<[Option<HotEntry>]>,
    hot_generation: u32,
    arena: Option<ExecutableArena>,
    entry_cap: usize,
    disabled: bool,
}

impl Default for BlockCache {
    fn default() -> Self {
        let page_len = super::exec_mem::host_page_len();
        Self::with_entry_cap(EXECUTABLE_ARENA_LEN / page_len)
    }
}

impl BlockCache {
    fn with_entry_cap(entry_cap: usize) -> Self {
        Self {
            entries: HashMap::new(),
            blocks: Vec::new(),
            hot: vec![None; HOT_LOOKUP_LEN].into_boxed_slice(),
            hot_generation: 1,
            arena: None,
            entry_cap,
            disabled: false,
        }
    }

    pub(crate) fn probe(&mut self, key: BlockKey) -> BlockProbe {
        if self.disabled {
            return BlockProbe::Rejected;
        }
        let hot_index = key.hot_index();
        if let Some(hit) = self.hot[hot_index] {
            if hit.generation == self.hot_generation && hit.key == key {
                return BlockProbe::Ready(self.blocks[usize::from(hit.id.0)]);
            }
        }
        match self.entries.get(&key).copied() {
            Some(BlockState::Compiled(id)) => {
                self.hot[hot_index] = Some(HotEntry {
                    key,
                    id,
                    generation: self.hot_generation,
                });
                BlockProbe::Ready(self.blocks[usize::from(id.0)])
            }
            Some(BlockState::Seen) => BlockProbe::Compile,
            Some(BlockState::Rejected) => BlockProbe::Rejected,
            None => {
                if self.entries.len() == self.entry_cap {
                    self.reset_storage();
                }
                self.entries.insert(key, BlockState::Seen);
                BlockProbe::Interpret
            }
        }
    }

    /// Install bytes produced after `probe` returned `Compile`.
    pub(crate) fn install(
        &mut self,
        span: BlockSpan,
        fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
        raw_clocks: u32,
        code: &[u8],
    ) -> Option<CompiledBlock> {
        if self.disabled || self.entries.get(&span.key) != Some(&BlockState::Seen) {
            return None;
        }
        let page_len = self
            .arena
            .as_ref()
            .map_or_else(super::exec_mem::host_page_len, ExecutableArena::slot_len);
        if code.is_empty() || code.len() > page_len {
            return None;
        }
        if self.arena.as_ref().is_some_and(ExecutableArena::is_full) {
            self.reset_storage();
            self.entries.insert(span.key, BlockState::Seen);
        }
        if self.arena.is_none() {
            self.arena = ExecutableArena::new();
        }
        let Some(entry) = self.arena.as_mut().and_then(|arena| arena.install(code)) else {
            self.disabled = true;
            return None;
        };
        let id = BlockId(u16::try_from(self.blocks.len()).ok()?);
        let block = CompiledBlock {
            span,
            entry: entry as usize,
            fetch_lens,
            raw_clocks: u16::try_from(raw_clocks).ok()?,
        };
        self.blocks.push(block);
        self.entries.insert(span.key, BlockState::Compiled(id));
        self.hot[span.key.hot_index()] = Some(HotEntry {
            key: span.key,
            id,
            generation: self.hot_generation,
        });
        Some(block)
    }

    /// Prevent repeated compilation attempts for a block the emitter cannot handle.
    pub(crate) fn reject(&mut self, key: BlockKey) {
        if self.entries.get(&key) == Some(&BlockState::Seen) {
            self.entries.insert(key, BlockState::Rejected);
        }
    }

    pub(crate) fn clear(&mut self) {
        // CS reloads and monitor transitions can invalidate code millions of times while the
        // direct cache is unused. Avoid clearing the 65,536-entry hot table when it is already
        // empty.
        if self.entries.is_empty() && self.blocks.is_empty() && self.arena.is_none() {
            self.disabled = false;
            return;
        }
        self.reset_storage();
        self.disabled = false;
    }

    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }

    #[cfg(test)]
    pub(crate) fn tracked_len(&self) -> usize {
        self.entries.len()
    }

    fn reset_storage(&mut self) {
        self.entries.clear();
        self.blocks.clear();
        self.hot_generation = self.hot_generation.wrapping_add(1);
        if self.hot_generation == 0 {
            self.hot.fill(None);
            self.hot_generation = 1;
        }
        self.arena = None;
    }
}

impl PartialEq for BlockCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for BlockCache {}

impl Clone for BlockCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for BlockCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlockCache {{ {} blocks }}", self.len())
    }
}

pub(crate) type DirectEntryFn = unsafe extern "C" fn(*mut CpuGsw, u32) -> u32;

pub(crate) struct Compilation {
    pub span: BlockSpan,
    pub fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub raw_clocks: u32,
    pub code: Vec<u8>,
}

#[derive(Clone, Copy)]
struct DirectInsn {
    lin: u32,
    len: u8,
    kind: DirectKind,
}

#[derive(Clone, Copy)]
enum DirectKind {
    MovReg { dst: u8, src: u8 },
    MovImm { dst: u8, imm: u32 },
    AluReg { op: u8, dst: u8, src: u8 },
    AluImm { op: u8, dst: u8, imm: u32 },
    Test { a: u8, b: u8 },
    Shift { op: u8, dst: u8, count: u8 },
    Jcc { condition: u8, taken_delta: u32 },
}

impl DirectKind {
    fn raw_clocks(self) -> u32 {
        if matches!(self, Self::Jcc { .. }) {
            3
        } else {
            2
        }
    }
}

const GUEST_HOMES: [Reg; 8] = [
    Reg::R8,
    Reg::R9,
    Reg::R10,
    Reg::R11,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::RBX,
];
const SAVED_HOST_REGS: [Reg; 7] = [
    Reg::RBX,
    Reg::RBP,
    Reg::RDI,
    Reg::R12,
    Reg::R13,
    Reg::R14,
    Reg::R15,
];
const ARITH_FLAGS: u32 = crate::FLAG_CF
    | crate::FLAG_PF
    | crate::FLAG_AF
    | crate::FLAG_ZF
    | crate::FLAG_SF
    | crate::FLAG_OF;
const LOGIC_FLAGS: u32 = ARITH_FLAGS & !crate::FLAG_AF;

#[cfg(target_os = "windows")]
const CPU_ARG: Reg = Reg::RCX;
#[cfg(not(target_os = "windows"))]
const CPU_ARG: Reg = Reg::RDI;
#[cfg(target_os = "windows")]
const FLAGS_ARG: Reg = Reg::RDX;
#[cfg(not(target_os = "windows"))]
const FLAGS_ARG: Reg = Reg::RSI;

pub(crate) fn key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<BlockKey> {
    if !super::HOST_SUPPORTED || !d || !matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586)
    {
        return None;
    }
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    // The first direct slice has no page-kind guard in emitted code. Keep video and ROM code on
    // the interpreter until the shared fast map can prove a page is ordinary RAM.
    if (0x000a_0000..0x0010_0000).contains(&physical) {
        return None;
    }
    Some(BlockKey::new(lin, physical, cpu.jit_mode_key()))
}

pub(crate) fn compile(cpu: &CpuGsw, entry_lin: u32, d: bool) -> Option<Compilation> {
    let key = key_for(cpu, entry_lin, d)?;
    let mut slots = Vec::with_capacity(MAX_BLOCK_INSTRUCTIONS);
    let mut fetch_lens = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut lin = entry_lin;
    let mut raw_clocks = 0u32;

    while slots.len() < MAX_BLOCK_INSTRUCTIONS {
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            break;
        };
        if insn.prefixes != Prefixes::default() || !insn.continuable {
            break;
        }
        let Some(next) = lin.checked_add(u32::from(insn.len)) else {
            break;
        };
        if entry_lin >> 12 != next.wrapping_sub(1) >> 12 {
            break;
        }
        let Some(expected_phys) = key.physical.checked_add(lin.wrapping_sub(entry_lin)) else {
            break;
        };
        if cpu.decode_cache.line_phys_start(lin, d) != Some(expected_phys) {
            break;
        }
        // This first engine owns complete register-only blocks. If a continuable instruction in
        // the middle needs a helper, reject the whole block so the legacy region engine can take it.
        let kind = classify(&insn, lin, entry_lin)?;
        raw_clocks += kind.raw_clocks();
        fetch_lens[slots.len()] = insn.len;
        slots.push(DirectInsn {
            lin,
            len: insn.len,
            kind,
        });
        lin = next;
        if matches!(kind, DirectKind::Jcc { .. }) {
            break;
        }
    }

    if slots.len() < 3 {
        return None;
    }
    let last = slots.last()?;
    let guest_len = last
        .lin
        .wrapping_add(u32::from(last.len))
        .wrapping_sub(entry_lin) as usize;
    let span = BlockSpan::new(key, guest_len, slots.len())?;
    let code = emit(&slots, span, raw_clocks);
    Some(Compilation {
        span,
        fetch_lens,
        raw_clocks,
        code,
    })
}

fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> Option<DirectKind> {
    if insn.operand_size != OperandSize::Dword {
        return None;
    }
    let opcode = u8::try_from(insn.opcode).ok();
    if let Some(opcode) = opcode {
        if opcode < 0x40 {
            let op = (opcode >> 3) & 7;
            let form = opcode & 7;
            if !matches!(op, 2 | 3) {
                match form {
                    1 => {
                        let m = insn.modrm?;
                        let DecodedOperand::Reg(dst) = insn.operand? else {
                            return None;
                        };
                        return Some(DirectKind::AluReg {
                            op,
                            dst,
                            src: m.reg,
                        });
                    }
                    3 => {
                        let m = insn.modrm?;
                        let DecodedOperand::Reg(src) = insn.operand? else {
                            return None;
                        };
                        return Some(DirectKind::AluReg {
                            op,
                            dst: m.reg,
                            src,
                        });
                    }
                    5 => {
                        return Some(DirectKind::AluImm {
                            op,
                            dst: 0,
                            imm: insn.imm,
                        });
                    }
                    _ => {}
                }
            }
        }
        match opcode {
            0x81 | 0x83 => {
                let m = insn.modrm?;
                if matches!(m.reg, 2 | 3) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::AluImm {
                    op: m.reg,
                    dst,
                    imm: insn.imm,
                });
            }
            0x85 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Test { a, b: m.reg });
            }
            0x89 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::MovReg { dst, src: m.reg });
            }
            0x8b => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(src) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::MovReg { dst: m.reg, src });
            }
            0xb8..=0xbf => {
                return Some(DirectKind::MovImm {
                    dst: opcode - 0xb8,
                    imm: insn.imm,
                });
            }
            0xc1 | 0xd1 => {
                let m = insn.modrm?;
                if !matches!(m.reg, 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Shift {
                    op: m.reg,
                    dst,
                    count: if opcode == 0xd1 { 1 } else { insn.imm as u8 },
                });
            }
            0x70..=0x7f if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jcc {
                    condition: opcode & 0x0f,
                    taken_delta: end_delta.wrapping_add(insn.imm),
                });
            }
            _ => {}
        }
    }
    if matches!(insn.opcode, 0x0f80..=0x0f8f) && insn.group == DecodeGroup::Branch {
        let end_delta = lin
            .wrapping_add(u32::from(insn.len))
            .wrapping_sub(entry_lin);
        return Some(DirectKind::Jcc {
            condition: (insn.opcode & 0x0f) as u8,
            taken_delta: end_delta.wrapping_add(insn.imm),
        });
    }
    None
}

fn emit(slots: &[DirectInsn], span: BlockSpan, raw_clocks: u32) -> Vec<u8> {
    let mut e = Encoder::new();
    for reg in SAVED_HOST_REGS {
        e.push(reg);
    }
    e.mov_r64_r64(Reg::R15, CPU_ARG);
    e.mov_r32_r32(Reg::RBP, FLAGS_ARG);
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.load_r32_disp32(home, Reg::R15, gpr_offset(index));
    }

    for slot in slots {
        match slot.kind {
            DirectKind::MovReg { dst, src } => {
                e.mov_r32_r32(home(dst), home(src));
            }
            DirectKind::MovImm { dst, imm } => e.mov_r32_imm32(home(dst), imm),
            DirectKind::AluReg { op, dst, src } => {
                emit_alu(&mut e, op, dst, Some(src), None);
            }
            DirectKind::AluImm { op, dst, imm } => {
                emit_alu(&mut e, op, dst, None, Some(imm));
            }
            DirectKind::Test { a, b } => emit_test(&mut e, a, b),
            DirectKind::Shift { op, dst, count } => emit_shift(&mut e, op, dst, count),
            DirectKind::Jcc {
                condition,
                taken_delta,
            } => {
                emit_store_homes(&mut e);
                emit_load_host_flags(&mut e);
                let taken = e.label();
                e.jcc(condition, taken);
                emit_return(&mut e, u32::from(span.guest_len), raw_clocks);
                e.place(taken);
                emit_return(&mut e, taken_delta, raw_clocks);
                return e.finish();
            }
        }
    }
    emit_store_homes(&mut e);
    emit_return(&mut e, u32::from(span.guest_len), raw_clocks);
    e.finish()
}

fn home(index: u8) -> Reg {
    GUEST_HOMES[usize::from(index & 7)]
}

fn gpr_offset(index: usize) -> i32 {
    (core::mem::offset_of!(CpuGsw, registers)
        + core::mem::offset_of!(Registers, gpr)
        + index * core::mem::size_of::<u32>()) as i32
}

fn eip_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip)) as i32
}

fn eflags_offset() -> i32 {
    (core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eflags)) as i32
}

fn pending_offset() -> i32 {
    core::mem::offset_of!(CpuGsw, pending_flags) as i32
}

fn emit_alu(e: &mut Encoder, op: u8, dst: u8, src: Option<u8>, imm: Option<u32>) {
    e.mov_r32_r32(Reg::RAX, home(dst));
    if let Some(src) = src {
        e.mov_r32_r32(Reg::RCX, home(src));
    } else {
        e.mov_r32_imm32(Reg::RCX, imm.expect("register or immediate source"));
    }
    let writes = op != 7;
    let target = if writes {
        home(dst)
    } else {
        e.mov_r32_r32(Reg::RDX, Reg::RAX);
        Reg::RDX
    };
    let host_op = if op == 7 { 5 } else { op };
    e.alu_r32_r32(host_op, target, Reg::RCX);

    if matches!(op, 1 | 4 | 6) {
        emit_capture_flags(e, LOGIC_FLAGS);
        emit_pending(e, 0x8000_0202, None, None, target);
        emit_logic_live_af(e);
    } else {
        emit_capture_flags(e, ARITH_FLAGS);
        let tag = if op == 0 { 0x8000_0200 } else { 0x8000_0201 };
        emit_pending(e, tag, Some(Reg::RAX), Some(Reg::RCX), target);
    }
}

fn emit_test(e: &mut Encoder, a: u8, b: u8) {
    e.mov_r32_r32(Reg::RDX, home(a));
    e.alu_r32_r32(4, Reg::RDX, home(b));
    emit_capture_flags(e, LOGIC_FLAGS);
    emit_pending(e, 0x8000_0202, None, None, Reg::RDX);
    emit_logic_live_af(e);
}

fn emit_shift(e: &mut Encoder, op: u8, dst: u8, raw_count: u8) {
    let count = raw_count & 0x1f;
    if count == 0 {
        return;
    }
    e.shift_r32_imm8(op, home(dst), count);
    let mut defined = crate::FLAG_CF | crate::FLAG_PF | crate::FLAG_ZF | crate::FLAG_SF;
    if count == 1 {
        defined |= crate::FLAG_OF;
    }
    emit_capture_flags(e, defined);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RBP);
    emit_clear_pending(e);
}

fn emit_capture_flags(e: &mut Encoder, defined: u32) {
    e.pushfq();
    e.pop(Reg::RDI);
    e.and_r32_imm32(Reg::RBP, !defined);
    e.and_r32_imm32(Reg::RDI, defined);
    e.or_r32_r32(Reg::RBP, Reg::RDI);
}

fn emit_load_host_flags(e: &mut Encoder) {
    e.mov_r32_r32(Reg::RAX, Reg::RBP);
    e.and_r32_imm32(Reg::RAX, ARITH_FLAGS | 0x2);
    e.push(Reg::RAX);
    e.popfq();
}

fn emit_logic_live_af(e: &mut Encoder) {
    e.load_r32_disp32(Reg::RDI, Reg::R15, eflags_offset());
    e.and_r32_imm32(Reg::RDI, !crate::FLAG_AF);
    e.mov_r32_r32(Reg::RDX, Reg::RBP);
    e.and_r32_imm32(Reg::RDX, crate::FLAG_AF);
    e.or_r32_r32(Reg::RDI, Reg::RDX);
    e.or_r32_imm32(Reg::RDI, 0x2);
    e.store_r32_disp32(Reg::R15, eflags_offset(), Reg::RDI);
}

fn emit_pending(e: &mut Encoder, tag: u32, a: Option<Reg>, b: Option<Reg>, result: Reg) {
    let base = pending_offset();
    e.store_u32_imm_disp32(Reg::R15, base, tag);
    if let Some(a) = a {
        e.store_r32_disp32(Reg::R15, base + 4, a);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 4, 0);
    }
    if let Some(b) = b {
        e.store_r32_disp32(Reg::R15, base + 8, b);
    } else {
        e.store_u32_imm_disp32(Reg::R15, base + 8, 0);
    }
    e.store_r32_disp32(Reg::R15, base + 12, result);
}

fn emit_clear_pending(e: &mut Encoder) {
    let base = pending_offset();
    for offset in [0, 4, 8, 12] {
        e.store_u32_imm_disp32(Reg::R15, base + offset, 0);
    }
}

fn emit_store_homes(e: &mut Encoder) {
    for (index, home) in GUEST_HOMES.into_iter().enumerate() {
        e.store_r32_disp32(Reg::R15, gpr_offset(index), home);
    }
}

fn emit_return(e: &mut Encoder, eip_delta: u32, raw_clocks: u32) {
    if eip_delta != 0 {
        e.load_r32_disp32(Reg::RAX, Reg::R15, eip_offset());
        e.add_r32_imm32(Reg::RAX, eip_delta);
        e.store_r32_disp32(Reg::R15, eip_offset(), Reg::RAX);
    }
    e.mov_r32_imm32(Reg::RAX, raw_clocks);
    for reg in SAVED_HOST_REGS.into_iter().rev() {
        e.pop(reg);
    }
    e.ret();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(linear: u32) -> BlockKey {
        BlockKey::new(linear, 0x20_000 + (linear & 0xfff), 7)
    }

    #[test]
    fn span_is_bounded_and_page_local() {
        assert!(BlockSpan::new(key(0x1234), 64, MAX_BLOCK_INSTRUCTIONS).is_some());
        assert!(BlockSpan::new(key(0x1ff0), 17, 1).is_none());
        assert!(BlockSpan::new(key(0x1234), 1, MAX_BLOCK_INSTRUCTIONS + 1).is_none());
        assert!(BlockSpan::new(key(0x1234), 0, 1).is_none());
    }

    #[test]
    fn first_observation_interprets_and_second_compiles() {
        let mut cache = BlockCache::default();
        let key = key(0x1234);
        assert!(matches!(cache.probe(key), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key), BlockProbe::Compile));
        cache.reject(key);
        assert!(matches!(cache.probe(key), BlockProbe::Rejected));
    }

    #[test]
    fn capacity_pressure_clears_seen_entries() {
        let mut cache = BlockCache::with_entry_cap(2);
        let first = key(0x1000);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key(0x1100)), BlockProbe::Interpret));
        assert!(matches!(cache.probe(key(0x1200)), BlockProbe::Interpret));
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn reset_ignores_stale_hot_entries_without_clearing_the_table() {
        let mut cache = BlockCache::with_entry_cap(2);
        let first = key(0x1000);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
        assert!(matches!(cache.probe(first), BlockProbe::Compile));
        let span = BlockSpan::new(first, 1, 1).expect("one byte is page local");
        let mut fetch_lens = [0; MAX_BLOCK_INSTRUCTIONS];
        fetch_lens[0] = 1;
        cache
            .install(span, fetch_lens, 1, &[0xc3])
            .expect("block must install");
        let hot_index = first.hot_index();
        let stale = cache.hot[hot_index].expect("install fills the hot slot");

        cache.clear();

        assert!(
            cache.hot[hot_index].is_some(),
            "reset must not scan the hot table"
        );
        assert_ne!(stale.generation, cache.hot_generation);
        assert!(matches!(cache.probe(first), BlockProbe::Interpret));
    }

    #[cfg(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    #[test]
    fn hash_fallback_preserves_hot_slot_collisions() {
        let mut cache = BlockCache::default();
        let first = key(0x1000);
        let second = (0x1001..)
            .map(key)
            .find(|candidate| candidate.hot_index() == first.hot_index())
            .expect("the finite hot table must collide");

        for candidate in [first, second] {
            assert!(matches!(cache.probe(candidate), BlockProbe::Interpret));
            assert!(matches!(cache.probe(candidate), BlockProbe::Compile));
            let span = BlockSpan::new(candidate, 1, 1).expect("one byte is page local");
            let mut fetch_lens = [0; MAX_BLOCK_INSTRUCTIONS];
            fetch_lens[0] = 1;
            cache
                .install(span, fetch_lens, 1, &[0xC3])
                .expect("block must install");
        }

        assert!(matches!(cache.probe(first), BlockProbe::Ready(_)));
        assert!(matches!(cache.probe(second), BlockProbe::Ready(_)));
        assert_eq!(cache.len(), 2);
    }
}
