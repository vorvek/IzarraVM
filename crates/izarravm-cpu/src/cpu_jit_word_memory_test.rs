// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The rejected-row campaign's Slice 3: the sixteen-bit MEMORY rows.
//!
//! Three census rows, and between them both halves of the width hazard:
//!
//! | row | doom | quake | what it is |
//! |---|---:|---:|---|
//! | `0x0FB6` MOVZX memory word | 1,442,795 | 31,216 | a BYTE load into a WORD destination |
//! | `0xC7 /0` MOV memory word | 742,811 | 240 | a two-byte immediate store |
//! | `0x83` ALU memory word | 12,192 | 162,440 | a sixteen-bit read-modify-write |
//!
//! Every row runs the same guest bytes natively and through a BLOCK-FREE interpreter from
//! identical state and compares registers (EIP included), lazy flags, EFLAGS, the halt latch, core
//! clocks, bus clocks and the WHOLE of guest RAM. The tested opcode is MID-BLOCK on every row: an
//! opcode at a block's entry slot parks the block on the interpreter, so an entry-position fixture
//! certifies nothing.
//!
//! **Guest RAM is the load-bearing comparison here in a way it was not for Slice 2.** A sixteen-bit
//! store that widened to four bytes writes the right two and two more that the interpreter leaves
//! alone; every register, flag and clock still agrees. `memory_fill` puts a distinct byte at every
//! address for exactly that reason, and the whole-array compare in `compare_state` is what sees it.
//!
//! Two derived properties of `MemoryWidth::Word` are asserted rather than assumed, because both
//! were about to be taken on trust:
//!
//! * **The alignment guard is a population cut, not a formality.** `emit_wide_page_guard` refuses
//!   an odd address, so a misaligned word access side-exits and the interpreter runs it. That is
//!   observable state (a retirement count and an EIP), and `misaligned_word_*` pins it.
//! * **The code-watch guard cannot report a straddle at ALIGNED Word.** `emit_code_watch_table_
//!   branch` tests every granule the access spans, as one shifted mask against a 32-bit window
//!   over the page's watch bitmap. A 2-aligned two-byte access spans two one-byte granules whose
//!   bits are adjacent in that window, so the fixture below asserts the TRANSACTIONAL exit -- the
//!   store refused before any byte landed -- rather than a cross-word straddle, which this
//!   fixture's addresses cannot construct.
//!
//!   This bullet used to say something different and wrong on both clauses, and the correction is
//!   worth keeping visible because both errors were invisible from inside this file. It claimed
//!   the guard "probes the access's LAST BYTE as well as its first", which stopped being true when
//!   the guard moved to the window test; and it justified the claim with a "16-byte chunk", a
//!   granule size that has not existed since `CHUNK_SHIFT` went to 0 and granules became single
//!   bytes. Neither clause was about this slice's Word rows at all.
//!
//! Mutation record for this file. Nine, all applied by hand, observed, and restored; the failing
//! assertion quoted is the FIRST one each produced.
//!
//! | mutation | caught by | assertion |
//! |---|---|---|
//! | `emit_extend_write_back`'s Word arm -> `mov_r32_r32(home(dst), RDX)`, i.e. the pre-slice code | both `extending_*` rows | registers: EAX `0x0000_0034` against the interpreter's `0xdead_0034` |
//! | `emit_load_extend`'s `(Word, false)` source arm -> `load_r32_disp8` | `extending_loads_*` | registers, at `movzx r,m16 osz=dword` |
//! | `emit_store_value`'s Word arm -> `store_r32_disp8` | `the_word_immediate_store_*` | guest RAM, at `mov word [0x3010], 0x1234` |
//! | `emit_alu_mem_dest`'s Word write-back -> `store_r32_disp8` | `the_word_memory_alu_*` | guest RAM, at `add word [0x3010], 0x01` |
//! | `emit_alu_mem_dest`'s Word READ -> `load_r32_disp8` | `the_word_memory_alu_*` | lazy flags |
//! | `emit_alu_candidate`'s Word arm -> `alu_r32_r32` | `the_word_memory_alu_*` | lazy flags, at `add ..., 0x80` |
//! | `emit_commit_alu_candidate`'s Word `width_tag` -> `0x200` | `the_word_memory_alu_*` | lazy flags |
//! | `needs_alignment_guard` -> `!matches!(self, Byte \| Word)` | `a_misaligned_word_read_modify_write_still_exits_*` | retirement count, 3 where 1 is allowed. **Re-derived after guard 3**, and the row changed shape: it used to be caught by three fixtures at once, and two of those now run misaligned Word accesses NATIVELY. What still discriminates is the read-modify-write row, which lowers through a site guard 3 did not relax — so the mutation removes the only guard that row has |
//! | dropping `emit_watched_store_guard` from `emit_store`'s RAM arm | `a_watched_word_store_*` | retirement count, 3 where 1 is allowed |
//!
//! The first row is the one that matters most: it is the pre-slice emitter verbatim, so it says
//! this fixture would have refused the admission had it been written before the `dst_width` field
//! rather than with it.
//!
//! Two of these are worth reading as a pair. The write-back and the read mutations BOTH widen the
//! `0x83` row, and they fail on different assertions -- guest RAM for the write, lazy flags for the
//! read. Neither alone would have covered the other.
//!
//! A tenth mutation was tried, did not fail, and is recorded here in CORRECTED form. An earlier
//! version of this note called it "mis-aimed rather than a survivor" and attributed it to
//! `RmwIncDec`. Both halves of that were wrong, and top-tier review caught the attribution.
//!
//! The mutation is `emit_pending_inc_dec`'s Word `width_tag` -> `0x200`, reached by a string match
//! that hit the wrong one of two four-line-identical `width_tag` tables (the other is
//! `emit_commit_alu_candidate`'s, which IS this slice's and IS caught, row seven above).
//!
//! * **The attribution.** `emit_pending_inc_dec` has FOUR calling functions, not one:
//!   `emit_rmw_inc_dec` (memory RMW, Word or Dword from the kind), `emit_rmw_inc_dec_dword`
//!   (Dword literal), `emit_inc_dec_reg` (register INC/DEC -- two textual call sites, one per
//!   width arm) and `emit_inc_dec_reg8` (Byte literal). So the Word tag serves `RmwIncDec` at Word
//!   AND `IncDecReg` at Word, and both are production-reachable: `0xff` and `0x40..=0x4f` are both
//!   on classify's Word allowlist.
//! * **It is a genuine SURVIVOR, not merely off-target.** The original check ran only the
//!   `word_memory` filter. Re-run against the whole crate it still passes: **1313 passed, 0
//!   failed.** Nothing in the tree covers the lazy-flag descriptor WIDTH of a sixteen-bit INC or
//!   DEC, so `66 FF /0` (INC m16) and `66 40` (INC AX) would build a descriptor claiming a dword
//!   operation over word operands, and the divergence only surfaces when a later instruction reads
//!   a flag lazily.
//!
//! That gap is PRE-EXISTING and outside this slice -- nothing here touches INC or DEC -- so it is
//! flagged for its own change rather than fixed in passing. It is written down because the two
//! tables are byte-identical for four lines and the next person to mutate one will land on the
//! wrong one exactly as this did.
//!
//! **CLOSED** by `cpu_jit_word_inc_dec_test.rs`, and the verdict there is worth carrying back: the
//! emitted code was CORRECT and only unmeasured, so that file is a pure test addition and no
//! emitted byte moved. The same mutation now fails four tests and passes 1324, the exact
//! pre-addition baseline.
//!
//! Two more mutations, applied against the B2 form-3 memory-SOURCE row
//! (`the_word_memory_source_alu_matches_the_interpreter_for_every_admitted_op`) and restored:
//!
//! | mutation | caught by | assertion |
//! |---|---|---|
//! | `emit_alu_preloaded`'s Word write-back -> `mov_r32_r32(home(dst), RDX)` | the memory-SOURCE row | registers: EAX `0x0000_3435` against the interpreter's `0xdead_3435`, at `add r16 dst=0 reg=0x0001, word [0x3010] operand=0x1234` |
//! | `emit_alu_preloaded`'s source mask `and_r32_imm32(RCX, 0xffff)` -> `0xffff_ffff` | NOT this row -- `word_alu_register_forms_*` and `word_alu_immediate_forms_*` | lazy flags: `b: 3203334145` against `1`, at `0x01 /0 add cx,ax` |
//!
//! The first is the miscompile the old form-3 refusal existed to prevent, and it fails on the
//! poisoned high half, which is why the seed carries one.
//!
//! The second is recorded for the SHAPE of its escape rather than for the catch. It survives this
//! row for the same reason the review gave for not mutating the READ: at a memory source RCX is
//! filled by `movzx_r32_word_disp8` and is ALREADY zero-extended, so masking it again is a no-op
//! here. It is the register and immediate forms -- where RCX carries a full 32-bit register or a
//! sign-extended imm8 -- that the mask is load-bearing for, and they catch it immediately.
//!
//! For the same reason, mutating the row's own read (`movzx_r32_word_disp8` -> `load_r32_disp8`)
//! canNOT fail HERE and was not attempted: the `and RAX, 0xffff` / `and RCX, 0xffff` pair discards
//! the extra bytes, and the bus charge is static from `word_reads` rather than from the host load's
//! width. Recorded so nobody retries it as the "stronger" mutation.
//!
//! That is a statement about THIS FIXTURE'S OPERANDS, not about the lane. Every address these rows
//! read is page-INTERIOR, so the two extra bytes a four-byte host load would touch are inside the
//! same mapped page. A guest Word read of the last two bytes of a FastMap page is a different
//! matter entirely -- the widened host load would read past the page the entry covers -- and the
//! emitted guard that keeps it from doing so is the page-CROSSING bound, which
//! `cpu_jit_misaligned_memory_test.rs` pins at the page edge on this very slot. Nobody should read
//! the paragraph above as "the read's width does not matter".
//!

use super::*;

/// `mov esi,esi`, the leading slot that keeps the tested opcode off the block entry.
const MOV_ESI_ESI: [u8; 2] = [0x89, 0xf6];
/// `mov edi,edi`, the trailing slot, so the tested opcode is never the last one either.
const MOV_EDI_EDI: [u8; 2] = [0x89, 0xff];

/// The word operand's linear address: 2-ALIGNED, on a page of its own, and inside the fixture's
/// 0x5000 of RAM. Its own page keeps it away from both the code at `ENTRY` and the stack at
/// `STACK_TOP`, so the only mapping that can serve it is the one `build` makes.
const OPERAND: u32 = 0x3010;
/// The same operand ONE byte higher, i.e. misaligned. Used only by the alignment rows.
const MISALIGNED_OPERAND: u32 = 0x3011;

/// A distinct byte at every address, so a store of the wrong WIDTH changes guest RAM even when it
/// writes the right value at the right place. A constant fill would make a four-byte store
/// indistinguishable from a two-byte one whenever the immediate's upper half happened to match.
fn memory_fill() -> Vec<u8> {
    let mut memory = vec![0u8; 0x5000];
    for (i, byte) in memory.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(37).wrapping_add(11) & 0xff) as u8;
    }
    memory
}

/// The architectural state both roles start from.
///
/// `gpr` poisons every register's HIGH half with a value neither role may touch. That is the whole
/// point for the MOVZX rows: a lowering that writes 32 bits where the operand size says 16 clears
/// it, and nothing else in the comparison would notice.
#[derive(Clone, Copy)]
struct Seed {
    gpr: [u32; 8],
    eflags: u32,
    live_pending: bool,
    /// Written as two little-endian bytes at `OPERAND` before the run, over the fill.
    operand: u16,
    /// A `mark_code_range` over this address (one byte) on BOTH roles, before compilation.
    watch: Option<u32>,
}

impl Seed {
    fn new() -> Self {
        Self {
            // 0xdead in every high half; the low half is the register index, so a row that reads
            // the wrong register is a distinguishable failure rather than a coincidence.
            gpr: std::array::from_fn(|i| 0xdead_0000 | (0xa0 + i as u32)),
            eflags: 0x202,
            live_pending: false,
            operand: 0x1234,
            watch: None,
        }
    }

    fn gpr(mut self, index: usize, value: u32) -> Self {
        self.gpr[index] = value;
        self
    }

    fn flags(mut self, eflags: u32) -> Self {
        self.eflags = eflags;
        self
    }

    fn pending(mut self) -> Self {
        self.live_pending = true;
        self
    }

    fn operand(mut self, operand: u16) -> Self {
        self.operand = operand;
        self
    }

    fn watch(mut self, at: u32) -> Self {
        self.watch = Some(at);
        self
    }
}

struct Roles {
    native: CpuGsw,
    native_bus: TestBus,
    interp: CpuGsw,
    interp_bus: TestBus,
    block: jit::direct::CompiledBlock,
    /// Linear address of the tested opcode, i.e. where a guarded exit must leave EIP.
    body_at: u32,
}

/// Map one page for read and write on the fast map. A memory-form slot silently never compiles
/// without this and the fixture then certifies a refusal it did not intend to test.
fn map_page(cpu: &mut CpuGsw, bus: &mut TestBus, page: u32) {
    for write in [false, true] {
        let kind = if write {
            BusAccessKind::DataWrite
        } else {
            BusAccessKind::DataRead
        };
        let host = bus.direct_page(page, kind).unwrap().unwrap();
        let ok = if write {
            cpu.jit_fast_map.populate_write(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        } else {
            cpu.jit_fast_map.populate_read(
                page,
                page,
                host,
                jit::fast_map::PagePermissions::UNPAGED,
                cpu.physical_page_watched(page),
            )
        };
        assert!(ok, "page {page:#x} must map");
    }
}

/// Compile `mov esi,esi / body / mov edi,edi / hlt` at `ENTRY` on the native role, warm the same
/// decode lines on the interpreter role, and seed both identically.
fn build(body: &[u8], seed: Seed) -> Roles {
    let mut code = MOV_ESI_ESI.to_vec();
    let body_at = ENTRY + code.len() as u32;
    code.extend_from_slice(body);
    let tail_at = ENTRY + code.len() as u32;
    code.extend_from_slice(&MOV_EDI_EDI);
    code.push(0xf4);

    let mut memory = memory_fill();
    // A NOP before the entry, so the block is reachable as a continuation as well as directly.
    memory[(ENTRY - 1) as usize] = 0x90;
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    memory[OPERAND as usize..OPERAND as usize + 2].copy_from_slice(&seed.operand.to_le_bytes());
    memory[MISALIGNED_OPERAND as usize..MISALIGNED_OPERAND as usize + 2]
        .copy_from_slice(&seed.operand.to_le_bytes());

    let mut native = flat_cpu();
    let mut interp = flat_cpu();
    let mut native_bus = TestBus::with_memory(memory.clone());
    let mut interp_bus = TestBus::with_memory(memory);
    for bus in [&mut native_bus, &mut interp_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let starts = [ENTRY, body_at, tail_at];
    for (cpu, bus) in [
        (&mut native, &mut native_bus),
        (&mut interp, &mut interp_bus),
    ] {
        cpu.registers.set_esp(STACK_TOP);
        for &linear in &starts {
            cpu.set_eip(linear);
            cpu.fetch_decoded(bus, linear).unwrap();
        }
        // BOTH roles are watched, not only the native one. The watch is guest-visible through the
        // interpreter's own SMC bookkeeping, so watching one role would compare two different
        // machines and the disagreement would be the fixture's, not the lowering's.
        //
        // Marked BEFORE `map_page`: the mark's E1 sweep invalidates any live fast-map entry on
        // the marked physical page whose PAGE_WATCHED bit is clear, so marking after populating
        // would immediately clear the entry `map_page` just installed (populate-then-mark trap).
        if let Some(at) = seed.watch {
            cpu.mark_decode_code_for_test(at, 1);
        }
        map_page(cpu, bus, OPERAND & !0xfff);
        map_page(cpu, bus, (STACK_TOP - 4) & !0xfff);
    }

    let key = jit::direct::key_for(&native, ENTRY, true).expect("entry key");
    assert!(matches!(
        native.jit_direct.probe(key),
        jit::direct::BlockProbe::Interpret
    ));
    let compilation = match jit::direct::compile(&mut native, ENTRY, true) {
        jit::direct::CompileOutcome::Compiled(compilation) => compilation,
        jit::direct::CompileOutcome::StructuralReject(_) => {
            panic!("structurally rejected: the word memory form is still a barrier")
        }
        jit::direct::CompileOutcome::Retry(_) => panic!("compile asked for a retry"),
    };
    assert_eq!(
        compilation.span.instructions, 3,
        "the block must cover all three slots, so the tested opcode really ran natively"
    );
    let id = native
        .jit_direct
        .install(&compilation)
        .expect("block installs");
    let block = native.jit_direct.block(id).expect("live block");

    for cpu in [&mut native, &mut interp] {
        cpu.halted = false;
        cpu.interrupt_shadow = false;
        cpu.registers.gpr = seed.gpr;
        cpu.registers.set_esp(STACK_TOP);
        cpu.registers.eflags = seed.eflags;
        cpu.pending_flags = PendingFlags::default();
        if seed.live_pending {
            // A descriptor produced BEFORE the tested instruction. MOVZX and MOV must leave it
            // alone; the `0x83` ALU forms must replace it with a WORD one.
            let _ = cpu.alu(0, 0x7fff_ffff, 1, BusWidth::Dword);
        }
        cpu.set_eip(ENTRY);
        cpu.elapsed_clocks = 0;
        cpu.timing_rem = 0;
        cpu.core_clocks_so_far = 0;
    }
    native_bus.trace = BusTrace::default();
    interp_bus.trace = BusTrace::default();

    Roles {
        native,
        native_bus,
        interp,
        interp_bus,
        block,
        body_at,
    }
}

fn compare_state(roles: &Roles, context: &str) {
    assert_eq!(
        roles.native.registers, roles.interp.registers,
        "{context}: registers"
    );
    assert_eq!(
        roles.native.eflags(),
        roles.interp.eflags(),
        "{context}: EFLAGS"
    );
    assert_eq!(
        roles.native.halted, roles.interp.halted,
        "{context}: halt latch"
    );
    assert_eq!(
        roles.native.elapsed_clocks, roles.interp.elapsed_clocks,
        "{context}: core clocks"
    );
    assert_eq!(
        roles.native_bus.trace.elapsed_clocks(),
        roles.interp_bus.trace.elapsed_clocks(),
        "{context}: bus clocks"
    );
    // The whole array, not a window around the operand. A store that widened writes bytes the
    // interpreter never touched, and a window sized to the intended access is exactly the wrong
    // shape to see that.
    assert_eq!(
        roles.native_bus.memory, roles.interp_bus.memory,
        "{context}: guest RAM"
    );
}

/// A row that completes NATIVELY: all three slots retire in the block and the whole architectural
/// state matches three interpreted steps.
fn lowered(body: &[u8], seed: Seed, context: &str) {
    let mut roles = build(body, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        3,
        "{context}: all three slots must retire natively"
    );
    for _ in 0..3 {
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, context);
}

/// A row whose emitted memory guard REFUSES. The native run must end at the tested opcode with the
/// instruction un-started -- no byte of guest RAM written, no register touched -- and the
/// interpreter must then execute it and reach the same state.
fn guarded(body: &[u8], seed: Seed, exits: fn(&CpuGsw) -> u64, context: &str) {
    let mut roles = build(body, seed);
    let retired = roles.native.perf_counters().jit_direct_insns;
    let before = exits(&roles.native);
    assert!(
        roles
            .native
            .try_run_direct_block_for_test(&mut roles.native_bus, roles.block)
            .unwrap(),
        "{context}: block did not run natively"
    );
    assert_eq!(
        roles.native.perf_counters().jit_direct_insns - retired,
        1,
        "{context}: only the slot BEFORE the tested opcode may retire natively"
    );
    assert_eq!(
        exits(&roles.native) - before,
        1,
        "{context}: exactly one side exit of the expected reason"
    );
    assert_eq!(
        roles.native.registers.eip, roles.body_at,
        "{context}: the run must end AT the tested opcode, not after it"
    );
    roles.interp.cycle(&mut roles.interp_bus).unwrap();
    compare_state(&roles, &format!("{context}: at the guard"));

    // Both roles now execute the refused instruction INTERPRETED and must agree, guest RAM
    // included: this is what says the guard was TRANSACTIONAL rather than half-applied.
    for _ in 0..2 {
        roles.native.cycle(&mut roles.native_bus).unwrap();
        roles.interp.cycle(&mut roles.interp_bus).unwrap();
    }
    compare_state(&roles, &format!("{context}: after the re-execution"));
}

fn alignment_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_cross_page_or_alignment
}

fn watch_exits(cpu: &CpuGsw) -> u64 {
    cpu.perf_counters().jit_direct_exit_code_watch
}

/// `[disp32]` addressing: ModRM mod 00, rm 101. No base register, so no row's operand address can
/// depend on the poisoned register seeds.
fn disp32(opcode_head: &[u8], reg: u8, at: u32) -> Vec<u8> {
    let mut body = opcode_head.to_vec();
    body.push((reg << 3) | 0b101);
    body.extend_from_slice(&at.to_le_bytes());
    body
}

// ---------------------------------------------------------------------------------------------
// `0x0FB6` / `0x0FB7` / `0x0FBE` / `0x0FBF` -- the extending loads
// ---------------------------------------------------------------------------------------------

/// The slice's headline row, and the property that admits it: at Word operand size MOVZX and MOVSX
/// define the destination's low 16 bits and PRESERVE its high 16.
///
/// Both operand sizes run in the same loop. The Dword control is not decoration: it is what says
/// the fixture would have caught the bug in the other direction too, i.e. a `dst_width` wired
/// backwards so that the unprefixed form stopped defining all 32 bits.
#[test]
fn extending_loads_write_the_operand_size_and_preserve_what_is_above_it() {
    for (opcode, name) in [
        (0xb6u8, "movzx r,m8"),
        (0xb7, "movzx r,m16"),
        (0xbe, "movsx r,m8"),
        (0xbf, "movsx r,m16"),
    ] {
        for word in [false, true] {
            // Every destination register, so a row that wrote `home(0)` regardless would fail.
            for dst in 0..8u8 {
                // Both sign classes of the source, since MOVSX's extension differs by the top bit
                // of the byte or word it reads.
                for operand in [0x1234u16, 0xfedc, 0x0080, 0x8000] {
                    let mut body = Vec::new();
                    if word {
                        body.push(0x66);
                    }
                    body.extend_from_slice(&disp32(&[0x0f, opcode], dst, OPERAND));
                    let label = format!(
                        "{name} dst={dst} operand={operand:#06x} osz={}",
                        if word { "word" } else { "dword" }
                    );
                    lowered(&body, Seed::new().operand(operand), &label);
                }
            }
        }
    }
}

/// The REGISTER form of the same four opcodes at Word operand size.
///
/// It shares one classifier arm with the memory form, so the allowlist entry admits both and the
/// arm cannot be widened for one without the other. Two shapes here are not reachable from the
/// memory rows above:
///
/// * a byte source in lane 4..=7, which is AH/CH/DH/BH -- bits 8-15 of `home(src - 4)`, and no
///   host home's second byte is addressable as an x86-64 high-byte register;
/// * `dst == src`, where the destination is also the source of the extension.
#[test]
fn extending_register_forms_write_the_operand_size_at_word() {
    for (opcode, name) in [
        (0xb6u8, "movzx r16,r8"),
        (0xb7, "movzx r16,r16"),
        (0xbe, "movsx r16,r8"),
        (0xbf, "movsx r16,r16"),
    ] {
        for dst in 0..8u8 {
            for src in 0..8u8 {
                let body = [0x66, 0x0f, opcode, 0b1100_0000 | (dst << 3) | src];
                // A source value whose every lane is distinguishable: the low byte, the second
                // byte (the AH lane) and the low word are all different and all have their top bit
                // set, so a sign extension of the wrong lane is visible.
                let seed = Seed::new().gpr(usize::from(src & 3), 0x1234_8fa7);
                lowered(&body, seed, &format!("{name} dst={dst} src={src}"));
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `0xC7 /0` -- MOV m16, imm16
// ---------------------------------------------------------------------------------------------

/// A two-byte immediate store writes exactly two bytes.
///
/// `memory_fill` and the whole-array compare in `compare_state` are what make this a real
/// assertion: with a zeroed fixture memory, a four-byte store of an immediate whose upper half is
/// zero writes the same bytes the interpreter leaves alone and nothing fails.
///
/// The immediates cover the two ways the width can leak. `decode` fetches this immediate with
/// `fetch_immediate(Word)`, which is `u32::from(fetch_u16(..))`, so `insn.imm`'s upper half is
/// ZERO whatever the guest bytes say -- a widened store would write zeros over the fill rather
/// than garbage, which is the quieter of the two failures and the one the fill is sized for.
#[test]
fn the_word_immediate_store_writes_exactly_two_bytes() {
    for imm in [0x1234u16, 0x0000, 0xffff, 0x8000] {
        let mut body = vec![0x66u8];
        body.extend_from_slice(&disp32(&[0xc7], 0, OPERAND));
        body.extend_from_slice(&imm.to_le_bytes());
        // The store must not consult, or disturb, the lazy descriptor or any flag.
        for (eflags, pending) in [(0x202u32, false), (0x8d5, true)] {
            let mut seed = Seed::new().flags(eflags);
            if pending {
                seed = seed.pending();
            }
            lowered(
                &body,
                seed,
                &format!("mov word [{OPERAND:#x}], {imm:#06x} pending={pending}"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// `0x83` memory word -- the sixteen-bit read-modify-write
// ---------------------------------------------------------------------------------------------

/// Every admitted sub-op of `83 /r ib` at Word operand size, memory destination.
///
/// This is the row that carries both halves of the hazard at once: the READ must not widen (or the
/// ALU and the lazy descriptor see a 32-bit operand) and the WRITE-BACK must touch exactly two
/// bytes. `/7` CMP is in the loop and writes nothing, which is the control that says a guest-RAM
/// failure on the other six is the write-back and not the read.
///
/// The operand values sit on the sixteen-bit boundaries a widened operation would sail past:
/// `0xffff + 1` carries out of sixteen bits and not out of thirty-two; `0x8000 - 1` and
/// `0x7fff + 1` are the signed overflow boundary at sixteen bits and nowhere near it at
/// thirty-two.
#[test]
fn the_word_memory_alu_matches_the_interpreter_for_every_admitted_sub_op() {
    for (op, name) in [
        (0u8, "add"),
        (1, "or"),
        (4, "and"),
        (5, "sub"),
        (6, "xor"),
        (7, "cmp"),
    ] {
        for operand in [0x1234u16, 0xffff, 0x8000, 0x7fff, 0x0000] {
            // Both signs of the sign-extended imm8, including the boundary values.
            for imm in [0x01u8, 0x7f, 0x80, 0xff] {
                for (eflags, pending) in [(0x202u32, false), (0x8d5, true)] {
                    let mut body = vec![0x66u8];
                    body.extend_from_slice(&disp32(&[0x83], op, OPERAND));
                    body.push(imm);
                    let mut seed = Seed::new().operand(operand).flags(eflags);
                    if pending {
                        seed = seed.pending();
                    }
                    lowered(
                        &body,
                        seed,
                        &format!(
                            "{name} word [{OPERAND:#x}], {imm:#04x} operand={operand:#06x} \
                             pending={pending}"
                        ),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Form 3 memory word -- the sixteen-bit memory-SOURCE ALU (the B2 peachdrm row)
// ---------------------------------------------------------------------------------------------

/// Every admitted opcode of the form-3 family at Word operand size, memory source.
///
/// This is "the pair" the form-3 classify comment names: Word width AND a register write-back
/// after a memory read. The read lane (`movzx_r32_word_disp8`) and the write-back lane
/// (`emit_alu_preloaded`'s `mov_r16_r16`) are each certified elsewhere; nothing exercises them
/// together until this row. `0x3b` CMP is in the loop and writes nothing, which is the control
/// that says a register failure on the other five is the write-back and not the read.
///
/// The seed poisons every high half with 0xdead, so a write-back that widens to 32 bits is a
/// distinguishable register failure, not a coincidence.
///
/// Two ADDRESSING shapes ride along at one row each per opcode and destination, because the matrix
/// itself is `[disp32]` throughout and that is one addressing mode out of many: a base-plus-
/// displacement form whose base is EBP (so the default segment is SS, not DS) and a segment-
/// OVERRIDE form through ES. Both share their address path with shipping Dword forms, so they are
/// expected green; what they close is the "only one addressing mode was ever exercised" gap, at
/// 240 rows rather than the 1,440 a third inner axis would have cost.
#[test]
fn the_word_memory_source_alu_matches_the_interpreter_for_every_admitted_op() {
    for (opcode, name) in [
        (0x03u8, "add"),
        (0x0b, "or"),
        (0x23, "and"),
        (0x2b, "sub"),
        (0x33, "xor"),
        (0x3b, "cmp"),
    ] {
        // dst 4 is ESP: `build`'s seed block overwrites it with `set_esp(STACK_TOP)` after
        // installing `seed.gpr`, so its poisoned high half is NOT a witness there. The row runs
        // for coverage of the home; the other three carry the write-back witness. The subset
        // keeps the row count at 480 where 0..8 would be 1,920 compile+run cycles.
        for dst in [0u8, 3, 4, 7] {
            for operand in [0x1234u16, 0xffff, 0x8000, 0x7fff, 0x0000] {
                // The two addressing shapes, once per (opcode, dst, operand) rather than on the
                // inner axes: the read they certify does not interact with the register seed or
                // the incoming flags, so repeating them there would buy nothing.
                //
                // `[ebp+0x10]` is ModRM mod 01 / rm 101, whose DEFAULT segment is SS rather than
                // DS. The fixture's SS is flat with base 0, so it resolves to the same operand --
                // which is the point: an address path that silently used the wrong segment base
                // would still land here, and what this row actually pins is that the base-plus-
                // displacement form compiles and reads the same two bytes.
                let base_disp = vec![0x66u8, opcode, 0x40 | (dst << 3) | 0b101, 0x10];
                // `es:` override on the plain [disp32] form. ES is flat here too.
                let mut es_override = vec![0x26u8, 0x66];
                es_override.extend_from_slice(&disp32(&[opcode], dst, OPERAND));
                for (body, shape) in [
                    (base_disp, "[ebp+0x10] (SS default)"),
                    (es_override, "es:[disp32]"),
                ] {
                    let seed = Seed::new()
                        .gpr(usize::from(dst), 0xdead_8000)
                        .gpr(5, OPERAND - 0x10)
                        .operand(operand);
                    lowered(
                        &body,
                        seed,
                        &format!("{name} r16 dst={dst}, word {shape} operand={operand:#06x}"),
                    );
                }
                for reg_low in [0x0001u16, 0x7fff, 0x8000, 0xffff] {
                    for (eflags, pending) in [(0x202u32, false), (0x8d5, true)] {
                        let mut body = vec![0x66u8];
                        body.extend_from_slice(&disp32(&[opcode], dst, OPERAND));
                        let mut seed = Seed::new()
                            .gpr(usize::from(dst), 0xdead_0000 | u32::from(reg_low))
                            .operand(operand)
                            .flags(eflags);
                        if pending {
                            seed = seed.pending();
                        }
                        lowered(
                            &body,
                            seed,
                            &format!(
                                "{name} r16 dst={dst} reg={reg_low:#06x}, word [{OPERAND:#x}] \
                                 operand={operand:#06x} pending={pending}"
                            ),
                        );
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The two guards, at two-byte width
// ---------------------------------------------------------------------------------------------

/// A MISALIGNED word operand at the sites guard 3 does NOT relax.
///
/// The three rows of this slice no longer agree, and the disagreement is the point. Guard 3 split
/// `emit_wide_page_guard` into a page-crossing half and an alignment half, and relaxed the
/// alignment half at the two LEAN one-lookup sites only:
///
/// * `add word [odd], imm8` is `AluMemDest`, a read-modify-write, which is site 6 and still
///   refuses. An RMW slot needs a read deposit and a write deposit inside one slot, which is its
///   own change.
/// * `movzx bx, word [odd]` is a `Load` through the lean read site and now RUNS NATIVELY. Its row
///   moved to `misaligned_memory`, whose harness asserts the split bus charge as an exact delta;
///   it cannot live here, because this module's `lowered` asserts bus clocks EQUAL to the
///   interpreter and a natively-served misaligned access deliberately charges more than
///   `TestBus`'s non-splitting slow path does.
/// * `mov word [odd], imm16` is a `Store` through the lean store site and moved with it.
///
/// What survives here is the row that still refuses, kept in this module because the `0x83` word
/// forms are what this file exists to certify.
///
/// The 386 admits misaligned accesses architecturally; refusing them natively is a missed lowering
/// rather than a divergence, which is exactly what the interpreted re-execution below proves.
#[test]
fn a_misaligned_word_read_modify_write_still_exits_to_the_interpreter() {
    let mut alu = vec![0x66u8];
    alu.extend_from_slice(&disp32(&[0x83], 0, MISALIGNED_OPERAND));
    alu.push(0x03);

    guarded(
        &alu,
        Seed::new(),
        alignment_exits,
        "add word [odd], imm8 (AluMemDest, site 6)",
    );
}

/// A word STORE into a watched chunk exits transactionally.
///
/// The guard tests every granule the access spans as one shifted mask, so a two-byte access whose
/// first byte is marked is watched whatever its alignment -- see the module docs, and note that
/// the "last-byte probe" this comment used to appeal to has not existed since the guard became a
/// window test. What this row certifies is the part that is not a tautology: the exit happens
/// BEFORE any byte is written, so `compare_state` at the guard sees the operand untouched, and the
/// interpreter's own re-execution then produces whatever the SMC path produces on both roles
/// alike.
#[test]
fn a_watched_word_store_exits_before_writing() {
    let mut store = vec![0x66u8];
    store.extend_from_slice(&disp32(&[0xc7], 0, OPERAND));
    store.extend_from_slice(&0x1234u16.to_le_bytes());

    let mut alu = vec![0x66u8];
    alu.extend_from_slice(&disp32(&[0x83], 5, OPERAND));
    alu.push(0x03);

    for (body, name) in [
        (store, "mov word [watched], imm16"),
        (alu, "sub word [watched], imm8"),
    ] {
        guarded(&body, Seed::new().watch(OPERAND), watch_exits, name);
    }
}
