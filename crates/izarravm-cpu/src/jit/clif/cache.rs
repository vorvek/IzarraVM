// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Track C C1a: the clif unit cache and the unit-boundary growth walker. A parallel type to
//! `jit::direct::BlockCache` (plan decision D-C1.1), reusing Direct's DESIGN: the same key
//! fields (section 2.1), the same static exclusions K1-K5 (section 2.2), and the same
//! classifier for unit growth (F-A5), while keeping clif's compile path decoupled from
//! Direct's emission internals. C1a units are SIDE-EXIT-PER-INSTRUCTION shells (review
//! finding F-A1, Option B): no lowering, an empty static timing profile, every instruction
//! retired by the interpreter after the side exit.

use std::collections::HashMap;

use super::super::direct::{self, MAX_BLOCK_INSTRUCTIONS, SegmentLayout, UnitTerminal};
use crate::{CpuGsw, CpuPersona, Prefixes, U32BuildHasher};

/// The clif unit key: the same three fields as `jit::direct::BlockKey` with identical
/// semantics (plan section 2.1), as a parallel type per decision D-C1.1.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ClifUnitKey {
    pub(crate) linear: u32,
    pub(crate) physical: u32,
    pub(crate) mode_key: u32,
}

/// K1-K5 from the entry-guard checklist (plan section 2.2), byte-for-byte the same
/// exclusions as `direct::key_for`: host support, decode variant, persona class, the BIOS
/// F-page window, and the video/ROM physical window (G4's static half).
pub(crate) fn clif_key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<ClifUnitKey> {
    if !super::super::host_supported()
        || !d
        || !matches!(cpu.persona(), CpuPersona::I486 | CpuPersona::I586)
    {
        return None;
    }
    if lin.wrapping_sub(0x000f_f000) < 0x400 {
        return None;
    }
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    if (0x000a_0000..0x0010_0000).contains(&physical) {
        return None;
    }
    Some(ClifUnitKey {
        linear: lin,
        physical,
        mode_key: cpu.jit_mode_key(),
    })
}

/// Per-unit descriptor: the clif analogue of Direct's `Compilation`/`CompiledBlock`
/// metadata. The guest byte layout here is AUTHORITATIVE (decision D-C1.5), never the
/// arena's page-rounded span. Carries the mode/CS/CPL/data-segment snapshot Direct's
/// `CompiledBlock` carries, plus the three guard/quota fields the review named explicitly:
/// `has_wide_accesses`, `is_self_loop`, and `instructions`.
#[derive(Clone, Debug)]
pub(crate) struct ClifUnitDescriptor {
    pub(crate) key: ClifUnitKey,
    /// Guest bytes covered, from the unit's own layout (D-C1.5).
    pub(crate) guest_len: u16,
    /// Per-instruction guest fetch lengths, `instructions` entries live.
    pub(crate) fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) instructions: u8,
    /// CS/data-segment descriptor snapshot at admission (guards G6/G8).
    pub(crate) segment_layout: SegmentLayout,
    /// CPL-3 memory model captured at admission (guard G7).
    pub(crate) memory_cpl3: bool,
    /// Any word or dword access in the unit (guard G9).
    pub(crate) has_wide_accesses: bool,
    /// Terminal Jcc whose taken target is the unit entry.
    pub(crate) is_self_loop: bool,
    /// The shell's native entry, once compiled and installed (C1a: a side-exit shell).
    pub(crate) entry: usize,
}

/// The result of the unit-boundary growth walk (F-A5).
pub(crate) struct UnitLayout {
    pub(crate) guest_len: u16,
    pub(crate) fetch_lens: [u8; MAX_BLOCK_INSTRUCTIONS],
    pub(crate) instructions: u8,
    pub(crate) has_wide_accesses: bool,
    pub(crate) is_self_loop: bool,
    pub(crate) read_segments: u8,
    pub(crate) write_segments: u8,
}

/// Forward-decode a unit from the hot root, terminating on the four terminal kinds
/// (Call/Jmp/Ret/Jcc, INCLUDED as the unit's last instruction), on the first structurally
/// unclassifiable opcode (Q1's stop-growth resolution: the unit ends BEFORE it), and at
/// `MAX_BLOCK_INSTRUCTIONS`, reusing `direct::classify` unchanged through the
/// `unit_growth_classify` seam. Page-local and warm-line-only, like Direct's builder; a
/// unit with zero instructions does not exist (`None`).
pub(crate) fn walk_unit(cpu: &CpuGsw, entry_lin: u32, d: bool) -> Option<UnitLayout> {
    let entry_page = entry_lin & !0xfff;
    let mut lin = entry_lin;
    let mut fetch_lens = [0u8; MAX_BLOCK_INSTRUCTIONS];
    let mut instructions = 0usize;
    let mut guest_len = 0u32;
    let mut has_wide_accesses = false;
    let mut is_self_loop = false;
    let mut read_segments = 0u8;
    let mut write_segments = 0u8;
    while instructions < MAX_BLOCK_INSTRUCTIONS {
        if lin & !0xfff != entry_page {
            break;
        }
        let Some(insn) = cpu.decode_cache.get(lin, d) else {
            break;
        };
        if insn.prefixes != Prefixes::default() {
            break;
        }
        if (lin & 0xfff) + u32::from(insn.len) > 0x1000 {
            break;
        }
        let Some(step) = direct::unit_growth_classify(&insn, lin, entry_lin) else {
            break;
        };
        fetch_lens[instructions] = insn.len;
        instructions += 1;
        guest_len += u32::from(insn.len);
        has_wide_accesses |= step.wide_access;
        read_segments |= step.read_segments;
        write_segments |= step.write_segments;
        lin = lin.wrapping_add(u32::from(insn.len));
        match step.terminal {
            Some(UnitTerminal::Jcc { taken_delta }) => {
                is_self_loop = taken_delta == 0;
                break;
            }
            Some(UnitTerminal::Jmp | UnitTerminal::Call | UnitTerminal::Ret) => break,
            None => {}
        }
    }
    if instructions == 0 {
        return None;
    }
    Some(UnitLayout {
        guest_len: u16::try_from(guest_len).ok()?,
        fetch_lens,
        instructions: instructions as u8,
        has_wide_accesses,
        is_self_loop,
        read_segments,
        write_segments,
    })
}

/// Admission states, mirroring the Direct cache's `BlockState` roles: `Seen` (first
/// encounter recorded), `Compiled` (a live unit descriptor), `Dormant` (parked; G1 heat
/// demotion or a failed install).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClifUnitState {
    Seen,
    Compiled(u32),
    Dormant,
}

/// The clif unit cache (decision D-C1.1): key-based lookup plus per-unit descriptors. No
/// hot-path accelerator table yet: a C1a shell unit executes nothing natively, so every
/// entry immediately side-exits and the lookup is never on a hot native-to-native path;
/// the accelerator is deferred to the sub-slice whose units actually retire instructions
/// (recorded deviation, mirrors the "if warranted" wording of the deliverable).
#[derive(Default)]
pub(crate) struct ClifUnitCache {
    entries: HashMap<ClifUnitKey, ClifUnitState, U32BuildHasher>,
    units: Vec<ClifUnitDescriptor>,
    /// C1a diagnostics, excluded from CpuGsw equality through the enclosing type (F-A8).
    pub(crate) units_admitted: u64,
    pub(crate) heat_demotions: u64,
}

impl ClifUnitCache {
    pub(crate) fn state(&self, key: ClifUnitKey) -> Option<ClifUnitState> {
        self.entries.get(&key).copied()
    }

    pub(crate) fn note_seen(&mut self, key: ClifUnitKey) {
        self.entries.entry(key).or_insert(ClifUnitState::Seen);
    }

    pub(crate) fn park_dormant(&mut self, key: ClifUnitKey) {
        self.entries.insert(key, ClifUnitState::Dormant);
        self.heat_demotions = self.heat_demotions.saturating_add(1);
    }

    /// Install a compiled shell descriptor for a key previously recorded `Seen`.
    pub(crate) fn install(&mut self, descriptor: ClifUnitDescriptor) -> Option<u32> {
        if self.entries.get(&descriptor.key) != Some(&ClifUnitState::Seen) {
            return None;
        }
        let index = u32::try_from(self.units.len()).ok()?;
        let key = descriptor.key;
        self.units.push(descriptor);
        self.entries.insert(key, ClifUnitState::Compiled(index));
        self.units_admitted = self.units_admitted.saturating_add(1);
        Some(index)
    }

    pub(crate) fn unit(&self, index: u32) -> Option<&ClifUnitDescriptor> {
        self.units.get(index as usize)
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.units.clear();
    }
}
