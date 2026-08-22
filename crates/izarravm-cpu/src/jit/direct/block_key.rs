// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The Direct backend's block key, and the two screens that decide whether an address may have one
//! at all.
//!
//! Moved out of `jit/direct.rs` verbatim for that file's source-line ceiling. No behaviour change:
//! the only edits are the visibility widenings the module boundary forces and the imports.

use izarravm_core::{CpuPersona, GswMode};

use super::{HOT_LOOKUP_LEN, word_operands_admitted};
use crate::CpuGsw;

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

    pub(crate) const fn linear(self) -> u32 {
        self.linear
    }

    pub(super) fn hot_index(self) -> usize {
        self.linear as usize & (HOT_LOOKUP_LEN - 1)
    }
}

/// May the Direct backend key a block at all, on this host and this persona? The screen
/// `key_for_phys` opened with, lifted to a function of `mode` alone so `CpuGsw` can cache it
/// (`JitState::native_keys_admitted`) and so the cache and the thing it caches are ONE expression
/// — the same discipline `fast_map_population_enabled` and its serve gate keep.
///
/// This does NOT subsume `word_operands_admitted`, and the two must not be merged. That predicate
/// answers a per-BLOCK question (does this segment's operand size survive the compile walk?) and
/// its coupling contract with the compile walk is documented on it; this one answers a per-CPU
/// question that the walk never re-asks. `key_for_phys` still consults both, in that order, so
/// the two walks keep answering identically.
pub(crate) fn native_keys_admitted(mode: GswMode) -> bool {
    crate::jit::host_supported() && matches!(mode.persona(), CpuPersona::I486 | CpuPersona::I586)
}

pub(crate) fn key_for(cpu: &CpuGsw, lin: u32, d: bool) -> Option<BlockKey> {
    let physical = cpu.decode_cache.line_phys_start(lin, d)?;
    key_for_phys(cpu, lin, d, physical)
}

/// `key_for` for a caller that already holds the line's physical start (a `DecodeLineView` taken
/// this iteration). Identical decision: the only thing `key_for` reads off the decode cache is
/// that one field, and `line_phys_start` would return exactly this value for the same key.
pub(crate) fn key_for_phys(cpu: &CpuGsw, lin: u32, d: bool, physical: u32) -> Option<BlockKey> {
    // Hoisted screen, read from the cache instead of recomputed. See
    // `JitState::native_keys_admitted` for why the cached answer cannot be stale; the assert
    // below is the enforcement, not the argument.
    debug_assert_eq!(
        cpu.jit_direct.native_keys_admitted,
        native_keys_admitted(cpu.mode()),
        "native_keys_admitted cache is stale relative to the host/persona screen; a mode mutator \
         is missing a refresh_native_key_admission() call"
    );
    if !cpu.jit_direct.native_keys_admitted {
        return None;
    }
    // A 16-bit code segment is admitted wherever `word_operands_admitted` says Word operands are
    // lowered, which since the 486 measurement is I486 and I586 BY DEFAULT. Every instruction in
    // such a segment decodes at `OperandSize::Word` (the size follows CS.D, not the opcode), so
    // where the policy refuses, the whole population would reach `classify`, fail on its FIRST
    // slot, and install a rejected span plus a physical-page watch for every hot 16-bit boundary.
    // Refusing the key here instead keeps that persona byte-identical by construction.
    //
    // The 16-bit population is real mode, V86 and 16-bit protected mode. V86 is deliberately IN,
    // and 16-bit V86 BLOCKS EXIST in the shipped configuration -- an earlier revision of this
    // comment said "no 16-bit block exists on any persona today", which was true while
    // `try_direct_continuation` refused every `!d` boundary and stopped being true when
    // `IZARRAVM_JIT16` defaulted to 1. The V86 conclusion now rests on per-opcode gates:
    //
    //   * `0xED`/`0xEE`/`0xEF`: no `classify` arm at any size, plus the Word allowlist. Two
    //     gates, either sufficient.
    //   * `0xEC` (IN AL,DX): the Word allowlist NO LONGER excludes it, and that is deliberate.
    //     Its call-out helper `port_read_al_dx` now SUPPORTS the V86 / CPL>IOPL state instead of
    //     refusing it: a pure phase (TLB hits only, an uncharged RAM peek) proves the TSS
    //     I/O-permission answer with no effect at all, and only then does a committed phase
    //     charge and read the port, in the interpreter's own order. Anything the pure phase
    //     cannot answer -- a TLB miss above all -- still returns the instruction to the
    //     interpreter unexecuted. `run_direct_block`'s entry refusal is UNCHANGED and still
    //     refuses dispatcher entries into a call-out-bearing block in that state; a CHAINED
    //     entry bypasses it by construction, which is the mechanism the slice is for. See
    //     `jit/direct/callout.rs` and the note on `classify`'s `0xec` arm.
    //   * PUSHF: its PUSHFD arm is refused by `stack_width_kind` in V86 (`StoreSource::Flags`,
    //     IOPL check), and its Word form is off the allowlist.
    //   * POPF, STI, INT, IRET: no `classify` arm at any size. That absence is PINNED by
    //     `v86_sensitive_opcodes_keep_their_word_answers` (cpu_jit_compile_outcome_test.rs),
    //     because an absence defended by nothing is exactly what a coverage campaign widens by
    //     accident.
    //   * CLI: an `InterpretOne` call-out since the S3 policy widening, so its V86 cover is the
    //     helper's fault arm rather than a compile-time refusal. `check_v86_iopl` is the first
    //     statement of the interpreter's own `0xfa` arm, which is the arm the helper runs, so a
    //     V86 task below IOPL 3 raises the same #GP from inside the call-out that it raised at
    //     the barrier. The same test pins that, from the other side.
    //
    // V86 blocks stay key-separated by mode-key bit 2.
    if !d && !word_operands_admitted(cpu) {
        return None;
    }
    if lin.wrapping_sub(0x000f_f000) < 0x400 {
        return None;
    }
    // The first direct slice has no page-kind guard in emitted code. Keep video and ROM code on
    // the interpreter until the shared fast map can prove a page is ordinary RAM.
    //
    // The spike's level 2 lifts the ROM half of that window only (see
    // `sixteen_bit_admission_level`): 0xC0000 and up is option ROM and the system BIOS, which is
    // read-only storage with no side effects, while 0xA0000..0xC0000 is the VGA aperture the
    // guard is really for and stays refused at every level.
    if (0x000a_0000..0x0010_0000).contains(&physical)
        && !(physical >= 0x000c_0000 && cpu.jit_direct.sixteen_bit_level >= 2)
    {
        return None;
    }
    Some(BlockKey::new(lin, physical, cpu.jit_mode_key()))
}
