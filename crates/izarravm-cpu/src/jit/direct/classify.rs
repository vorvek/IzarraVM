// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

pub(super) fn classify(insn: &DecodedInsn, lin: u32, entry_lin: u32) -> Option<DirectKind> {
    if insn.group == DecodeGroup::Fpu {
        if insn.operand_size != OperandSize::Dword {
            return None;
        }
        let native = NativeX87Insn::classify(insn)?;
        let addr = match native {
            NativeX87Insn::BinaryMemory { addr, .. }
            | NativeX87Insn::IntBinaryMemory { addr, .. }
            | NativeX87Insn::LoadF32 { addr }
            | NativeX87Insn::StoreF32 { addr, .. }
            | NativeX87Insn::LoadI32 { addr }
            | NativeX87Insn::StoreI32 { addr, .. }
            // Every variant whose `metadata().memory` is Some must appear here. The `_ => None`
            // below is silent: a missing arm leaves `addr: None`, `emit_x87_slot`'s
            // `addr.expect(..)` panics at block compilation, and behind that panic
            // `DirectKind::read_segment` would have dropped the segment from the block's
            // `SegmentLayout` mask and made `kind_segment_access_supported` trivially true.
            | NativeX87Insn::LoadControlWord { addr }
            | NativeX87Insn::StoreControlWord { addr }
            | NativeX87Insn::LoadF64 { addr }
            | NativeX87Insn::StoreF64 { addr, .. }
            | NativeX87Insn::BinaryMemoryF64 { addr, .. }
            | NativeX87Insn::LoadI64 { addr }
            | NativeX87Insn::StoreI64 { addr }
            | NativeX87Insn::StoreExtended80 { addr } => Some(direct_addr(addr)?),
            _ => None,
        };
        return Some(DirectKind::X87 { insn: native, addr });
    }
    let operand_width = match insn.operand_size {
        OperandSize::Word => MemoryWidth::Word,
        OperandSize::Dword => MemoryWidth::Dword,
    };
    // The Jcc ranges are the only control transfers admitted at Word size. Both are matched on
    // the FULL u16 opcode here, above the `u8::try_from(insn.opcode)` truncation further down, so
    // `0x0f80..=0x0f8f` is well-typed and `0x70..=0x7f` cannot alias the two-byte 0x0f7x block the
    // way it would below the truncation.
    //
    // A Word-size relative branch masks its target to 16 bits, and the emitted form bakes an
    // unmasked delta. What makes that safe is the compile loop's `control_target_limit` clamp,
    // which refuses any Word control target above the wrap. Admitting a control transfer here
    // WITHOUT that clamp is a silent wrong-branch miscompile, not a missed lowering.
    // The BYTE-OPERAND opcodes below are admitted at Word size for a reason that is structural
    // rather than per-opcode, and it is worth stating once here instead of at each arm.
    //
    // `operand_size` is computed from CS.D and the 0x66 prefix ALONE and is opcode-independent
    // (`decode.rs`), so in a 16-bit code segment EVERY unprefixed instruction reports `Word`,
    // byte-operand forms included. This gate is therefore a blanket filter that catches them as
    // collateral. Admitting them changes nothing about how they lower, because their width is a
    // property of the FORM: each produces a kind carrying a literal `MemoryWidth::Byte`, or a
    // kind with no width at all. Nor can the operand size leak past this function: `DirectInsn`
    // carries only `lin`, `len`, `weighted_fp_clocks` and `kind`, and `EmitInput` carries no
    // `OperandSize` either, so every width decision downstream comes from the kind.
    //
    // The byte set is CLOSED over its shared classifier arms on purpose. `0x04..=0x3c` step 8 are
    // all `form == 4` of the ALU group and reach one arm; `0xf6` is the byte half of the
    // `0xf6 | 0xf7` group arm, whose every Dword-producing path is keyed `opcode == 0xf7`.
    // Admitting one member of a shared arm while refusing its sibling would be arbitrary, and
    // what makes 16-bit blocks link is a CONTIGUOUS admissible region rather than any single
    // opcode.
    //
    // `0x8c` is the one non-byte member and it is here for the same structural reason rather than
    // as an exception: its interpreter arm writes `OperandSize::Word` unconditionally, so the
    // 66-prefixed and unprefixed encodings have identical semantics and `MovSegToReg` carries no
    // width to get wrong. Its Dword-sibling hazard does not exist because it has no Dword sibling.
    //
    // Deliberately NOT here, and each would be a miscompile rather than a missed lowering:
    // `0xf7`, `0xa9`, `0xb8..=0xbf`, `0xc7`, `0x81`, `0x85`, `0x8d`, `0xa3`. Every one is the Dword
    // sibling of an admitted byte form and its kind hard-codes Dword with no width field.
    // `0x01`/`0x31` are worse still: `AluReg` does carry a width, but before the widening described
    // below `emit_alu_preloaded`'s Word branch ignored `op`, hard-coded SUB and wrote the result to
    // a scratch register instead of the destination, which is correct only for the CMP forms
    // `0x39`/`0x3b` already admitted here. That branch now handles the whole non-carry op set with
    // write-back, so `0x01`/`0x31` are a lowering waiting to be MEASURED rather than a hazard --
    // but they are not admitted in this slice, because nothing in the census asks for them and an
    // unmeasured admission is a formation change with no row to attribute it to.
    //
    // `0x83` WAS on that list and is now admitted, which is the second half of this slice. Its
    // register form produces `AluImm`, which carries a `width` field as of this commit, and its
    // memory form is refused inside the arm (see there). The census ranks `0x83 /5` word at
    // 9,776,289 doom exits, forty-seven apart from `0x60` PUSHAD -- one function prologue, so the
    // two must land together or neither's exits go anywhere.
    //
    // `0x81` stays OUT and the asymmetry is deliberate: its Word encoding carries a 16-bit
    // immediate that `decode` fetches with `fetch_u16` into the same `insn.imm`, so admitting it
    // would need the immediate path checked as well as the operation path. `0x83`'s immediate is a
    // sign-extended imm8 at BOTH operand sizes, one `fetch_u8`, so its encoding is width-invariant
    // and only the operation had to be widened.
    if insn.operand_size == OperandSize::Word
        && !matches!(
            insn.opcode,
            0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x34 | 0x39 | 0x3b | 0x3c
                | 0x40..=0x4f
                | 0x50..=0x5f
                | 0x68
                | 0x6a
                | 0x70..=0x7f
                | 0x80
                | 0x83
                | 0x84
                | 0x88
                | 0x89
                | 0x8a
                | 0x8b
                | 0x8c
                | 0x98
                | 0x99
                | 0xa8
                | 0xb0..=0xb7
                | 0xc2
                | 0xc3
                | 0xc6
                | 0xe8
                | 0xe9
                | 0xeb
                | 0x0f80..=0x0f8f
                | 0xf6
                | 0xff
        )
    {
        return None;
    }
    // SETcc r/m8, register destination. Byte-wide whatever the operand-size prefix says (the
    // interpreter's arm calls `write_operand_u8` without consulting `operand_size`), but 0x0f9x
    // is NOT in the Word-size allowlist above, so a 66-prefixed encoding never reaches here and
    // the point is moot rather than relied on. Keyed on the full u16 for the reason 0x0faf below
    // documents: the `u8::try_from` truncation further down cannot see a two-byte opcode.
    if let 0x0f90..=0x0f9f = insn.opcode {
        let DecodedOperand::Reg(dst) = insn.operand? else {
            return None;
        };
        return Some(DirectKind::SetCc {
            condition: (insn.opcode & 0x0f) as u8,
            dst,
        });
    }
    // IMUL r32, r/m32, both operand forms. Must stay below the Word-size gate above: a
    // 66-prefixed IMUL decodes with OperandSize::Word and is not in that gate's allowlist, so it
    // already falls through to `None` there. Moving this arm above the gate, or adding 0x0faf to
    // the allowlist, would silently lower a 16-bit IMUL as a 32-bit multiply instead: the
    // destination's high 16 bits would be clobbered rather than preserved, and CF/OF would be
    // computed against the wrong width.
    if insn.opcode == 0x0faf {
        // Keyed on the full u16 opcode rather than the u8 truncation further down: that
        // truncation (`u8::try_from(insn.opcode).ok()`) returns None for every two-byte opcode,
        // so the u8 arms below are unreachable for 0x0faf regardless. Matching the full u16 here
        // keeps that explicit and local instead of relying on the truncation's behavior.
        //
        // Both forms share ONE arm so the gate placement above cannot come to apply to one and
        // not the other. The `?` on `direct_addr` returns None from `classify`, not from the
        // match, which is what every other memory arm in this file does for an unsupported
        // address size or scale.
        let m = insn.modrm?;
        return match insn.operand? {
            DecodedOperand::Reg(src) => Some(DirectKind::Imul { dst: m.reg, src }),
            DecodedOperand::Mem(addr) => Some(DirectKind::ImulMem {
                dst: m.reg,
                addr: direct_addr(addr)?,
            }),
        };
    }
    // BT r/m32, r32, REGISTER form only. Keyed on the full u16 opcode and placed ABOVE the
    // `u8::try_from(insn.opcode).ok()` truncation for the same reason 0x0faf and the MOVZX/MOVSX
    // family are: that truncation returns None for every two-byte opcode, so an arm among the u8
    // arms below would be unreachable and nothing would fail, the lowering would simply never
    // fire.
    //
    // Must stay BELOW the OperandSize::Word gate. A 66-prefixed BT is not in that gate's
    // allowlist so it is already refused, and it must be: at Word the interpreter masks the bit
    // index with `& 15`, not `& 31` (`bits = operand_size.bytes() * 8`).
    //
    // Only 0xa3 of the four-opcode family, and only the register form. BTS/BTR/BTC (0xab, 0xb3,
    // 0xbb) WRITE the operand back; this arm's kind does not, and the interpreter skips the
    // write-back for op 0 alone. The memory form adjusts its effective address by the bit index
    // at runtime, which a static DirectAddr cannot express.
    if insn.opcode == 0x0fa3 {
        let m = insn.modrm?;
        return match insn.operand? {
            DecodedOperand::Reg(rm) => Some(DirectKind::Bt { rm, index: m.reg }),
            DecodedOperand::Mem(_) => None,
        };
    }
    // MOVZX and MOVSX, memory form only. Keyed on the full u16 opcode and placed ABOVE the
    // `u8::try_from(insn.opcode).ok()` truncation further down, for the same reason 0x0faf is:
    // that truncation returns None for every two-byte opcode, so an arm added among the u8 arms
    // below (next to the 0x8a/0x8b MOV forms it most resembles) would be UNREACHABLE. Nothing
    // would fail; the lowering would simply never fire, and only the pre-flight counter would
    // notice. It also has to stay BELOW the OperandSize::Word gate above: none of these four is in
    // that gate's allowlist, so a 66-prefixed form is already rejected there, and it must be,
    // because `write_gpr_sized` at Word merges into the low 16 bits instead of replacing all 32.
    //
    // `width` is the SOURCE width and comes from the sub-opcode, NOT from `operand_width`. That
    // local reflects the DESTINATION size and is Dword for every admitted form here, so using it
    // would turn every capture into a dword read.
    if matches!(insn.opcode, 0x0fb6 | 0x0fb7 | 0x0fbe | 0x0fbf) {
        let m = insn.modrm?;
        let width = if matches!(insn.opcode, 0x0fb6 | 0x0fbe) {
            MemoryWidth::Byte
        } else {
            MemoryWidth::Word
        };
        let signed = matches!(insn.opcode, 0x0fbe | 0x0fbf);
        // Both operand forms share ONE arm so the gate placement above cannot come to apply to
        // one and not the other. For the register form `src` is the raw ModRM rm field, which at
        // Byte width is a byte-register index where 4..=7 are AH/CH/DH/BH; the emitter reuses the
        // interpreter's own lane arithmetic rather than repeating it.
        let DecodedOperand::Mem(addr) = insn.operand? else {
            let DecodedOperand::Reg(src) = insn.operand? else {
                return None;
            };
            return Some(DirectKind::MovExtendReg {
                dst: m.reg,
                src,
                width,
                signed,
            });
        };
        return Some(DirectKind::LoadExtend {
            dst: m.reg,
            width,
            signed,
            addr: direct_addr(addr)?,
            // Every one of the four interpreter arms returns clocks(3) (execute.rs). The
            // DirectKind::raw_clocks default arm returns 2, which would undercharge each of these
            // by one clock and break byte identity on executed_cpu_core_clocks without failing any
            // unit test, so this is carried as a field the way Load and Store carry theirs.
            raw_clocks: 3,
        });
    }
    if matches!(insn.opcode, 0x0fa4 | 0x0fa5 | 0x0fac | 0x0fad) {
        let m = insn.modrm?;
        let count = if matches!(insn.opcode, 0x0fa4 | 0x0fac) {
            ShiftCount::Immediate(insn.imm as u8)
        } else {
            ShiftCount::Cl
        };
        let left = matches!(insn.opcode, 0x0fa4 | 0x0fa5);
        return match insn.operand? {
            DecodedOperand::Reg(dst) => Some(DirectKind::DoubleShiftReg {
                left,
                dst,
                src: m.reg,
                count,
            }),
            DecodedOperand::Mem(addr) => Some(DirectKind::DoubleShiftMem {
                left,
                src: m.reg,
                count,
                addr: direct_addr(addr)?,
            }),
        };
    }
    let opcode = u8::try_from(insn.opcode).ok();
    if let Some(opcode) = opcode {
        if opcode < 0x40 {
            let op = (opcode >> 3) & 7;
            let form = opcode & 7;
            match form {
                // Byte r/m destination, byte register source. Memory only: the register form
                // needs a byte-lane `AluReg` that does not exist yet, and the point of this arm
                // is the memory row.
                //
                // Width is a property of the form, not of the prefix — the interpreter's arm
                // reads `read_operand_u8`/`read_gpr8` and charges the same `clocks(2)` as every
                // other ALU form without consulting `operand_size` — so `MemoryWidth::Byte` is a
                // literal here rather than `operand_width`. 0x38 is deliberately NOT in the
                // Word-size allowlist above, so a 66-prefixed encoding never reaches this arm.
                0 => {
                    let m = insn.modrm?;
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::AluMemDest {
                        op,
                        source: StoreSource::Reg(m.reg),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                    });
                }
                1 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(dst) => Some(DirectKind::AluReg {
                            op,
                            dst,
                            src: m.reg,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                            op,
                            source: StoreSource::Reg(m.reg),
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                3 => {
                    let m = insn.modrm?;
                    return match insn.operand? {
                        DecodedOperand::Reg(src) => Some(DirectKind::AluReg {
                            op,
                            dst: m.reg,
                            src,
                            width: operand_width,
                        }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::AluMemSource {
                            op,
                            dst: m.reg,
                            width: operand_width,
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                // ALU accumulator forms with an imm8: ADD/OR/ADC/SBB/AND/SUB/XOR/CMP AL, imm8
                // (0x04/0x0c/0x14/0x1c/0x24/0x2c/0x34/0x3c). Semantically this is the 0x80 group
                // with the destination fixed to AL, so it reuses `AluByteImm` and
                // `emit_alu_byte_imm` unchanged; `dst: 0` is AL exactly as the interpreter's
                // `read_gpr8(0)`/`write_gpr8(0)` pair means it, and op 7 CMP suppresses the
                // writeback inside the emitter.
                //
                // This arm stays inside this `match form`, BELOW the OperandSize::Word gate near
                // the top of `classify`, and its placement is still load-bearing. What CHANGED:
                // the whole `0x04..=0x3c` family is now IN that gate's allowlist, so a Word-size
                // `3C ib` reaches this arm and is lowered as a byte op.
                //
                // That is correct, and it was checked against the interpreter rather than against
                // the architecture. `decode` fetches this immediate with an unconditional
                // `fetch_u8` for `form == 4` (only `form == 5` consults `operand_size`), and
                // `execute`'s matching arm uses `read_gpr8(0)`, `BusWidth::Byte`, `write_gpr8(0)`
                // and `clocks(2)` without ever reading `operand_size`. So a 66-prefixed `3C ib`
                // in 32-bit code and an unprefixed one at CS.D = 0 are the same operation on the
                // same lane for the same clocks. An earlier version of this comment warned that
                // admitting it "would lower a 16-bit-prefixed form as a byte op": that is true,
                // and it is what the interpreter does.
                //
                // It must not consult `operand_width`: byte width is a property of the form, not
                // of the prefix. It must not touch `insn.modrm` or `insn.operand` either, which
                // are None here because `decode` only parses a ModRM for forms below 4.
                4 => {
                    return Some(DirectKind::AluByteImm {
                        op,
                        dst: 0,
                        imm: insn.imm as u8,
                    });
                }
                // ALU accumulator forms with a full-width immediate (0x05/0x0d/.../0x3d). NOT in
                // the Word allowlist above, so `operand_width` is Dword whenever this is reached;
                // it is passed rather than hard-coded so the two `AluImm` producers state the width
                // the same way.
                5 => {
                    return Some(DirectKind::AluImm {
                        op,
                        dst: 0,
                        imm: insn.imm,
                        lane: None,
                        width: operand_width,
                    });
                }
                _ => {}
            }
        }
        match opcode {
            0x40..=0x4f => {
                return Some(DirectKind::IncDecReg {
                    dst: opcode & 7,
                    is_dec: opcode >= 0x48,
                    width: operand_width,
                });
            }
            // PUSHAD / POPAD, the MEMORY class of interpreter call-out slot. Ranked one and three
            // in the post-Phase-5 census (9,776,336 and 7,005,733 doom dispatcher exits) and
            // coupled to the `0x83 /5` word row below: the three are one function prologue and its
            // epilogue, so lowering any one alone only relocates its exits onto the next
            // instruction.
            //
            // A CALL-OUT rather than a lowering, and for a SIZE reason rather than a reachability
            // one -- emitted code can reach guest memory perfectly well; eight guarded accesses per
            // instruction is what does not fit a one-host-page block. `jit/direct/callout.rs`
            // carries the class table, the two-phase resident-then-move design, and a note on the
            // single-wide-guard shape that would beat this if the family is ever worth more wall.
            //
            // Dword only. Neither opcode is in the OperandSize::Word allowlist above, so PUSHA and
            // POPA (the 16-bit forms, which move sixteen bytes rather than thirty-two) fall to
            // `None` and stay barriers. The helper additionally refuses a 16-bit STACK (SS.B = 0)
            // at run time, which is an orthogonal axis the allowlist cannot see.
            //
            // No ModRM, no `insn.operand`, no operand of any kind: both are register-file-implicit.
            0x60 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PushAllDword,
                });
            }
            0x61 => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PopAllDword,
                });
            }
            0x50..=0x57 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Reg(opcode - 0x50),
                });
            }
            0x58..=0x5f => {
                return Some(DirectKind::Pop { dst: opcode - 0x58 });
            }
            // LEAVE. The 16-bit-stack form (SS.B = 0) moves only BP into SP and preserves
            // ESP's high word, which the emitted full-width move would destroy; that case is
            // refused at compile time by `uses_stack()` feeding the `stack_is_32bit` check,
            // NOT here. The 16-bit OPERAND-size form is refused by the OperandSize::Word
            // gate above, which does not list 0xc9.
            // NOP. Deliberately NOT added to the OperandSize::Word allowlist above, and that is
            // a measured decision rather than caution: `try_direct_continuation` returns
            // Interpret for every `!d` boundary before a key is ever built (`run.rs`), so no
            // 16-bit block exists on any persona today and the allowlist entry would be dead
            // code that no counter could gate. Admitting it there belongs to the banked 16-bit
            // admission work, not here.
            0x90 => {
                return Some(DirectKind::Nop);
            }
            // CBW / CWDE. Unlike NOP and CLD/STD immediately below, this one IS in the
            // Word-size allowlist above: the interpreter's arm switches on `operand_size` (the
            // Word case is CBW, Dword is CWDE), so a 16-bit block containing it is a real
            // lowering rather than dead code no counter could gate. Accumulator-implicit: no
            // ModRM, no `insn.operand`.
            0x98 => {
                return Some(DirectKind::Cwde {
                    width: operand_width,
                });
            }
            // IN AL, DX -- the FIRST and, this phase, the ONLY interpreter call-out slot. Not a
            // native lowering: the block spills, routes this one instruction through the
            // interpreter's port path (which needs the bus, and the bus is not reachable from
            // emitted code), reloads, and keeps running. See `jit/direct/callout.rs` for the
            // helper contract and the abnormal-set enumeration.
            //
            // Ranked here by the Phase 3 class table: exits to blocks rejected at this opcode's
            // barrier are the single largest identified share of doom's unbound static exit pool.
            //
            // Byte-wide and accumulator-implicit: no ModRM, no `insn.operand`, the port comes
            // from the live DX. The interpreter's 0xec arm does NOT consult `operand_size` (it
            // reads DX and writes AL at `BusWidth::Byte` unconditionally), so the form is
            // operand-size-invariant -- but 0xec is deliberately absent from the Word-size
            // allowlist above for the reason that list documents, so a 66-prefixed encoding falls
            // to `None` and stays a barrier. The invariance is stated, not relied on.
            //
            // The Approximate-class gate is INHERITED, not re-stated. `block_continuable`
            // (decode.rs) admits the IN forms only on I486/I586, so on the Accurate 386 class
            // `insn.continuable` is false and the compile walk stops at this instruction BEFORE
            // it ever reaches `classify`. That is what makes the 386 personas provably
            // byte-identical across this slice.
            0xec => {
                return Some(DirectKind::CallOut {
                    helper: CallOutHelper::PortReadAlDx,
                });
            }
            // CWD / CDQ, same reasoning as 0x98 immediately above.
            0x99 => {
                return Some(DirectKind::Cdq {
                    width: operand_width,
                });
            }
            // CLD / STD. Ranked third in the runtime-weighted reject audit at 1.37M dispatcher
            // exits (10.9% of rejected-target exits) despite being worth only ~0.06pp of
            // instruction coverage -- coverage share and dispatch-exit share are different
            // quantities, and an earlier slice dismissed this opcode on the wrong one.
            //
            // Deliberately NOT added to the OperandSize::Word allowlist above, for the reason
            // the NOP comment gives: no 16-bit block exists on any persona today, so the entry
            // would be dead code no counter could gate.
            0xfc | 0xfd => {
                return Some(DirectKind::DirectionFlag {
                    set: opcode == 0xfd,
                });
            }
            0xc9 => {
                return Some(DirectKind::Leave);
            }
            // PUSHFD. Fifth in the runtime-weighted reject audit at 1,194,127 dispatcher exits
            // (9.5%). The persona mask and the V86 refusal are resolved in `stack_width_kind`,
            // which has the CPU; `u32::MAX` is the placeholder until then and must never reach
            // the emitter.
            0x9c => {
                return Some(DirectKind::Push {
                    source: StoreSource::Flags { mask: u32::MAX },
                });
            }
            0x68 => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(insn.imm),
                });
            }
            0x6a => {
                return Some(DirectKind::Push {
                    source: StoreSource::Imm(crate::sign_extend_u8(insn.imm as u8)),
                });
            }
            0x80 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluByteImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::AluMemDest {
                        op: m.reg,
                        source: StoreSource::Imm(insn.imm),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0x81 | 0x83 => {
                let m = insn.modrm?;
                // ADC (/2) and SBB (/3) are refused at Word size and only there. They consume the
                // incoming CF as an operand, which the Dword path handles with a branch on the
                // EFLAGS shadow (`emit_carry_alu_preloaded`) that has no sixteen-bit twin. Refusing
                // is a missed lowering; the census measures zero Word `0x83 /2` and `/3`.
                if insn.operand_size == OperandSize::Word && matches!(m.reg, 2 | 3) {
                    return None;
                }
                return match insn.operand? {
                    // `lane: None` here is deliberate and not a stub: `classify` has no `&CpuGsw`
                    // and no physical address, and a lane needs both. The compile loop attaches
                    // one through `imm_lane_for` for the single shape that qualifies, exactly as
                    // `stack_width_kind` resolves the CPU-dependent stack forms after this point.
                    DecodedOperand::Reg(dst) => Some(DirectKind::AluImm {
                        op: m.reg,
                        dst,
                        imm: insn.imm,
                        lane: None,
                        width: operand_width,
                    }),
                    // The MEMORY form stays Dword-only, and the width is a literal rather than
                    // `operand_width` to say so. `emit_alu_mem_dest` reads its operand, runs
                    // `emit_alu_preloaded` and writes the result back through `emit_store_value` at
                    // the same width; the Word path through it has never been exercised and the
                    // read/modify/write triple would need its own differential row. Admitting the
                    // register form alone is what the census asks for -- `0x83 /5` word is a
                    // REGISTER row -- so the memory form is left where it is.
                    DecodedOperand::Mem(addr) if insn.operand_size == OperandSize::Dword => {
                        Some(DirectKind::AluMemDest {
                            op: m.reg,
                            source: StoreSource::Imm(insn.imm),
                            width: MemoryWidth::Dword,
                            addr: direct_addr(addr)?,
                        })
                    }
                    DecodedOperand::Mem(_) => None,
                };
            }
            0x84 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::TestByte { a, b: m.reg });
            }
            0x85 => {
                let m = insn.modrm?;
                let DecodedOperand::Reg(a) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Test { a, b: m.reg });
            }
            0xa8 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Byte,
                });
            }
            0xa9 => {
                return Some(DirectKind::TestImmReg {
                    dst: 0,
                    imm: insn.imm,
                    width: MemoryWidth::Dword,
                });
            }
            0x88 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovRegByte { dst, src: m.reg }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x89 => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovReg {
                        dst,
                        src: m.reg,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Reg(m.reg),
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8a => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovRegByte { dst: m.reg, src }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: MemoryWidth::Byte,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8b => {
                let m = insn.modrm?;
                return match insn.operand? {
                    DecodedOperand::Reg(src) => Some(DirectKind::MovReg {
                        dst: m.reg,
                        src,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Load {
                        dst: m.reg,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0x8c => {
                // MOV r/m16, Sreg. The interpreter writes `OperandSize::Word` unconditionally
                // ("always a word store regardless of operand size", execute.rs 0x8c), so this
                // kind carries no width and both the prefixed and unprefixed encodings lower
                // identically — which is what lets 0x8c join the Word-size allowlist above.
                // reg 6 and 7 are left to the interpreter deliberately. `segment_from_reg_field`
                // folds them into GS through a catch-all `_` arm rather than by intent, and
                // reproducing an accident is how a lowering and its oracle drift apart; 0..=5 are
                // the encodings with a named answer.
                let m = insn.modrm?;
                let segment = match m.reg {
                    0 => SegmentIndex::Es,
                    1 => SegmentIndex::Cs,
                    2 => SegmentIndex::Ss,
                    3 => SegmentIndex::Ds,
                    4 => SegmentIndex::Fs,
                    5 => SegmentIndex::Gs,
                    _ => return None,
                };
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::MovSegToReg { dst, segment });
            }
            0x8d => {
                let m = insn.modrm?;
                let DecodedOperand::Mem(addr) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::Lea {
                    dst: m.reg,
                    addr: direct_addr(addr)?,
                });
            }
            0xa0 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa1 => {
                return Some(DirectKind::Load {
                    dst: 0,
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa2 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Byte,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xa3 => {
                return Some(DirectKind::Store {
                    source: StoreSource::Reg(0),
                    width: MemoryWidth::Dword,
                    addr: DirectAddr {
                        segment: insn.prefixes.segment_override.unwrap_or(SegmentIndex::Ds),
                        base: None,
                        index: None,
                        scale: 1,
                        disp: insn.imm,
                    },
                    raw_clocks: 4,
                });
            }
            0xb0..=0xb7 => {
                return Some(DirectKind::MovImmByte {
                    dst: opcode - 0xb0,
                    imm: insn.imm as u8,
                });
            }
            0xb8..=0xbf => {
                return Some(DirectKind::MovImm {
                    dst: opcode - 0xb8,
                    imm: insn.imm,
                });
            }
            0xc6 | 0xc7 => {
                let m = insn.modrm?;
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xc6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) if opcode == 0xc6 => Some(DirectKind::MovImmByte {
                        dst,
                        imm: insn.imm as u8,
                    }),
                    DecodedOperand::Reg(dst) => Some(DirectKind::MovImm { dst, imm: insn.imm }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::Store {
                        source: StoreSource::Imm(insn.imm),
                        width,
                        addr: direct_addr(addr)?,
                        raw_clocks: 2,
                    }),
                };
            }
            0xc1 | 0xd1 => {
                let m = insn.modrm?;
                // reg 1 is ROR and MUST be admitted by this guard, not appended after it: reg 1
                // fails `matches!(m.reg, 4..=7)`, so a rotate arm placed below the guard would be
                // unreachable and the whole lowering would be dead code that no negative test
                // could detect. ROL (/0), RCL (/2) and RCR (/3) stay out. The refreshed attribution
                // measures zero rejects for all three, and RCL and RCR additionally take the
                // incoming CF as a rotate INPUT (`shift_rotate` seeds `cf` from `flag(FLAG_CF)`
                // before its loop), which would need the flags loaded into the host before the
                // rotate rather than only captured after it.
                if !matches!(m.reg, 1 | 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                // The RAW immediate, unmasked, matching what the Shift arm has always stored. The
                // architectural five-bit mask is applied in the emitter.
                let count = if opcode == 0xd1 { 1 } else { insn.imm as u8 };
                if m.reg == 1 {
                    return Some(DirectKind::RotateRightReg { dst, count });
                }
                return Some(DirectKind::Shift {
                    op: m.reg,
                    dst,
                    count,
                });
            }
            // Group 2 by CL. Sixth in the runtime-weighted reject audit: /7 alone is 807,607
            // dispatcher exits and the three shift sub-ops together are 7.9% of rejected-target
            // exits. Same /4..=7 admission as the imm8 arm's shift half; ROL (/0), ROR (/1),
            // RCL (/2) and RCR (/3) stay out -- ROR because its flag rules differ and the other
            // three for the imm8 arm's standing reasons (no measured rejects; RCL/RCR consume
            // the incoming CF as a rotate input the emitted form never loads).
            0xd3 => {
                let m = insn.modrm?;
                if !matches!(m.reg, 4..=7) {
                    return None;
                }
                let DecodedOperand::Reg(dst) = insn.operand? else {
                    return None;
                };
                return Some(DirectKind::ShiftCl { op: m.reg, dst });
            }
            0xf6 | 0xf7 => {
                let m = insn.modrm?;
                // NEG r/m32, register form. Deliberately carries NO width field: this arm sits
                // below the OperandSize::Word gate at the top of `classify`, and that allowlist
                // (which does not contain 0xf7) is the ONLY thing stopping a 586-mode `66 F7 /3`
                // from reaching here, since the persona gate admits word ops on 586 and
                // `prefixes_supported` accepts the operand-size override. A `width` field would
                // invite a future edit to pass `operand_width`, which is MemoryWidth::Word in
                // exactly that case, and a 16-bit NEG would then be lowered as a 32-bit one,
                // clobbering the destination's high half. Same hazard the 0x0faf comment above
                // describes. In a 16-bit segment the unprefixed form is Word (gated the same way)
                // and the 66-prefixed form is rejected earlier for carrying a prefix at all, so
                // NEG is simply never lowered there.
                if opcode == 0xf7 && m.reg == 3 {
                    let DecodedOperand::Reg(dst) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::NegReg { dst });
                }
                // MUL r/m32, register form. `reg == 4` is the UNSIGNED multiply; /5 next to it is
                // the signed IMUL, whose overflow rule is different (the product not sign-extending
                // back from the low half rather than the high half being nonzero), so this must not
                // widen to `4..=5`. Carries no width field for the same reason NegReg does not: the
                // OperandSize::Word gate above is the only thing keeping a 586-mode `66 F7 /4` out,
                // and a 16-bit MUL writes DX and AX as halves of the existing EDX and EAX rather
                // than replacing them.
                if opcode == 0xf7 && m.reg == 4 {
                    let DecodedOperand::Reg(src) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::MulReg { src });
                }
                // IMUL r/m32, one-operand SIGNED multiply, memory form. `reg == 5`, the signed
                // sibling of the /4 above, whose overflow rule is different: the product failing to
                // sign-extend back from the low half, rather than the high half being nonzero.
                //
                // ORDERING INVARIANT, and it is load-bearing rather than cosmetic. This arm MUST
                // stay BELOW the /4 arm. That arm's `else { return None }` returns from `classify`,
                // not from the arm, so a /4 with a MEMORY operand is already unreachable by the time
                // control gets here. Move this arm above it and widen either to `4..=5` and an
                // unsigned `mul dword [mem]` is emitted as a signed multiply, with the wrong EDX and
                // the wrong CF and OF. `mul_memory_form_stays_interpreter_only` is what catches it.
                //
                // `opcode == 0xf7` is equally load-bearing: this arm sits inside the shared
                // `0xf6 | 0xf7` group arm, and 0xF6 /5 is the BYTE IMUL, which multiplies AL and
                // writes only AX. Without the test it would be read as a dword and lowered as the
                // dword multiply. `imul_byte_form_stays_interpreter_only` is what catches that.
                //
                // No width field and no raw_clocks field. The OperandSize::Word gate above keeps a
                // 66-prefixed form out, and the whole group-3 arm returns clocks(2), which is
                // already the DirectKind::raw_clocks default. This is the opposite of 0x0FAF, where
                // the interpreter charges clocks(9) and the default undercharges by 7.
                if opcode == 0xf7 && m.reg == 5 {
                    return match insn.operand? {
                        // The REGISTER form, added by the rejected-row campaign's F7 slice. It
                        // shares this arm with the memory form rather than sitting in its own
                        // `if` because everything that makes the memory form correct here --
                        // `opcode == 0xf7` excluding the byte IMUL, the position BELOW /4
                        // excluding an unsigned multiply, the Word gate excluding a 66-prefixed
                        // encoding -- is exactly what makes the register form correct, and
                        // splitting them would invite one of the three to be re-derived wrongly.
                        DecodedOperand::Reg(src) => Some(DirectKind::ImulRegAcc { src }),
                        DecodedOperand::Mem(addr) => Some(DirectKind::ImulMemAcc {
                            addr: direct_addr(addr)?,
                        }),
                    };
                }
                // DIV (/6) and IDIV (/7) r/m32, REGISTER form. The two are ONE arm because the
                // emitter's two bodies are selected by `signed` and both are separately tested;
                // what must not be widened is the pair of ENCODER primitives behind them.
                //
                // MEMORY is deliberately absent, and the reason is the fault rather than the
                // address. A memory DIV can side-exit for two independent reasons -- the read's
                // own guards and the divide guard -- at the same slot, and the second must not be
                // reachable before the first has been proved not to fire. Both fixtures' memory
                // rows are under the campaign's 100k floor (doom 28,078; quake 95,839 + 57,491),
                // so the shape is left unbuilt rather than built untested.
                //
                // No `raw_clocks`: group 3 charges `clocks(2)` for every sub-opcode and both
                // operand forms, which is the `_ => 2` default. No `width`: the `OperandSize::Word`
                // gate at the top of `classify` excludes 0xf7, exactly as for `MulReg`.
                if opcode == 0xf7 && matches!(m.reg, 6 | 7) {
                    let DecodedOperand::Reg(src) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::DivReg {
                        src,
                        signed: m.reg == 7,
                    });
                }
                if m.reg != 0 {
                    return None;
                }
                let width = if opcode == 0xf6 {
                    MemoryWidth::Byte
                } else {
                    MemoryWidth::Dword
                };
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::TestImmReg {
                        dst,
                        imm: insn.imm,
                        width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::TestImmMem {
                        imm: insn.imm,
                        width,
                        addr: direct_addr(addr)?,
                    }),
                };
            }
            0xc2 | 0xc3 => {
                // No width gate here. `OperandSize` has exactly two variants, so the only widths
                // that reach this arm are Word and Dword, and both are wanted: the Word-size
                // allowlist above decides admission and the compile loop's stack-width matrix
                // rewrites the Word form into its own kind. A Byte check would read as live and
                // be provably unreachable.
                return Some(DirectKind::Ret {
                    release: if opcode == 0xc2 { insn.imm as u16 } else { 0 },
                });
            }
            // INC/DEC r/m8, REGISTER form only. The byte sibling of the 0xff group below.
            //
            // The memory form is deliberately absent: `emit_rmw_inc_dec` handles Dword and Word
            // and debug-asserts on the rest, and a Byte path needs its own code-watch width,
            // counter lane and the fact that a byte access takes NO alignment guard at all.
            //
            // `dst` here is a BYTE-REGISTER index, where 4..7 mean AH/CH/DH/BH rather than
            // ESP/EBP/ESI/EDI. The emitter's byte branch reads and writes through the lane
            // helpers for exactly that reason; a `home(dst)` on this value would hit the wrong
            // register entirely.
            0xfe => {
                let m = insn.modrm?;
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: MemoryWidth::Byte,
                    }),
                    DecodedOperand::Mem(_) => None,
                };
            }
            0xff => {
                let m = insn.modrm?;
                // /6 PUSH r/m32, memory form only. The REGISTER form is architecturally
                // `PUSH r32` and is refused: its clock charge would have to be checked against
                // 0x50..0x57 rather than assumed, and the attribution census measures zero
                // occurrences of it on this corpus. Refusing it is a missed lowering worth
                // nothing; mapping it onto `Push` without checking is a timing bug.
                //
                // /3 far CALL and /5 far JMP are not lowered here; both load a descriptor, which
                // needs machinery this classifier does not have.
                //
                // /2 CALL r32, REGISTER form only. The interpreter reads the target from the GPR
                // BEFORE the return EIP is pushed (execute_extended.rs, group 5 arm 2), which is
                // why the emit arm reloads home(dst) before the ESP adjust rather than after: the
                // register form is a dynamic-target control transfer, needing the same successor
                // machinery `Ret` and `JmpMem` use.
                //
                // Classified regardless of operand width; there is no width gate here the way /4
                // JMP has one. `CallReg` IS `uses_stack()`, so a 66-prefixed form routes into the
                // stack-width admission matrix in the compile loop, which refuses it for lack of a
                // `CallReg16` mapping arm, the same PushMem precedent that guards PUSH r/m32.
                //
                // The MEMORY form is not lowered: the census measures zero occurrences of it, and
                // lowering it would add a guarded load lane for nothing.
                if m.reg == 2 {
                    let DecodedOperand::Reg(dst) = insn.operand? else {
                        return None;
                    };
                    let return_delta = lin
                        .wrapping_add(u32::from(insn.len))
                        .wrapping_sub(entry_lin);
                    return Some(DirectKind::CallReg { dst, return_delta });
                }
                if m.reg == 6 {
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None;
                    };
                    return Some(DirectKind::PushMem {
                        addr: direct_addr(addr)?,
                    });
                }
                // /4 JMP r/m32, MEMORY form only. `0xff` is in the `OperandSize::Word` allowlist
                // above, so a 66-prefixed `FF /4` in 32-bit code reaches this arm at Word size.
                // NOTHING downstream refuses that: `uses_stack` is false for a jump, so the
                // stack-width admission matrix never sees this kind, and `static_control_target`
                // is `None` for a dynamic target, so the Word control clamp never sees it either.
                // This check is the only gate, on I586 (every other persona refuses Word before
                // reaching here). At Word size the interpreter reads TWO bytes and masks EIP to
                // 16 bits; lowering that as the Dword construction reads four bytes and jumps
                // unmasked, a miscompile twice over.
                if m.reg == 4 {
                    if insn.operand_size != OperandSize::Dword {
                        return None;
                    }
                    let DecodedOperand::Mem(addr) = insn.operand? else {
                        return None; // register form: census zero, PUSH-r32-style clock risk
                    };
                    return Some(DirectKind::JmpMem {
                        addr: direct_addr(addr)?,
                    });
                }
                if !matches!(m.reg, 0 | 1) {
                    return None;
                }
                return match insn.operand? {
                    DecodedOperand::Reg(dst) => Some(DirectKind::IncDecReg {
                        dst,
                        is_dec: m.reg == 1,
                        width: operand_width,
                    }),
                    DecodedOperand::Mem(addr) => Some(DirectKind::RmwIncDec {
                        is_dec: m.reg == 1,
                        width: operand_width,
                        addr: direct_addr(addr)?,
                    }),
                };
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
            0xe8 if insn.group == DecodeGroup::Branch => {
                let return_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Call {
                    return_delta,
                    target_delta: return_delta.wrapping_add(insn.imm),
                });
            }
            0xe9 | 0xeb if insn.group == DecodeGroup::Branch => {
                let end_delta = lin
                    .wrapping_add(u32::from(insn.len))
                    .wrapping_sub(entry_lin);
                return Some(DirectKind::Jmp {
                    target_delta: end_delta.wrapping_add(insn.imm),
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

pub(super) fn direct_addr(addr: crate::AddrMode) -> Option<DirectAddr> {
    // Both address sizes. The 16-bit modes already arrive in exactly this shape:
    // `parse_16bit_address` emits the eight register pairs as base/index at scale 1, with the
    // displacement sign-extended and SS selected for the BP forms. The 64K wrap is applied by the
    // emitter as a block property, because the address size is a pure function of CS.D.
    if !matches!(addr.scale, 1 | 2 | 4 | 8) {
        return None;
    }
    Some(DirectAddr {
        segment: addr.segment,
        base: addr.base,
        index: addr.index,
        scale: addr.scale,
        disp: addr.disp as u32,
    })
}
