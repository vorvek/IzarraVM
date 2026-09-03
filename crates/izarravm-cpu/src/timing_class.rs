// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The per-instruction charge CLASS and the three persona class tables.
//!
//! Slice 1a of the timing recalibration
//! (`dev_docs/2026-09-05-586-recalibration-design.md` §3.2, §9.4, §9.9; the
//! review's "Revision 2 re-review" item 1). Today every charge site in the
//! interpreter and the JIT carries a bare integer literal -- `Ok(clocks(2))`,
//! `DirectKind::raw_clocks`'s `_ => 2` -- and that literal is the SAME number
//! for all three personas, scaled afterwards by `level_timing`'s single dial.
//! This module replaces the literal with a NAME (`TimingClass`) plus a
//! per-persona, per-epoch lookup (`ClassTable`), so a persona can charge a
//! Pentium's count for `DIV r/m32` where the 386 charges the i386's.
//!
//! # The two epochs, and why epoch 1 is bit-identical BY CONSTRUCTION
//!
//! `IZARRAVM_TIMING_EPOCH` (read once at `Machine` construction; unset = 1) is
//! the only selector. Epoch 1 resolves to [`EPOCH1`] for **every** persona, and
//! every entry in [`EPOCH1`] is the literal the site used to carry. So an
//! epoch-1 run cannot differ from the pre-slice tree unless a routing mistake
//! maps a site to the wrong class -- which is exactly what
//! `timing_class_test.rs`'s epoch-1 fixture catches, literal by literal.
//!
//! Epoch 2's unit is the design's: `level_timing` stays `(1, 12)` and **one raw
//! clock is one twelfth of a core clock**, so every Intel count appears here
//! multiplied by 12 and is exact.
//!
//! # Provenance
//!
//! Every class carries a `provenance` string naming the document and row it
//! came from. Three spellings appear, and the difference is load-bearing:
//!
//! * `cmp §3 row N` -- `dev_docs/2026-09-05-86box-pentium-timing-comparison.md`
//!   §3's per-instruction table (Intel App. F of 241430-004, re-read there).
//! * `486 §5` -- `dev_docs/2026-09-05-486-timing-audit.md` §5's I486 column.
//! * `UNSOURCED x12` -- **no reference count was read for this class.** The
//!   epoch-2 entry is the epoch-1 literal times 12, which preserves today's
//!   relative cost and lands on the SLOW side (the owner's 12:15 ruling:
//!   "a miss on the slow side is a soft finding; a miss on the fast side is a
//!   hard failure"). These are the rows slice 4 must source or re-solve; the
//!   count is asserted in the tests so it cannot grow silently.
//!
//! # EPOCH 2 IS NOT YET COHERENT. Do not measure it.
//!
//! Slice 1a routes the INTERPRETER's `execute.rs` and `run.rs` charge sites onto
//! the table (131 of the 274). Three things are still on their literals:
//! `execute_extended.rs` and `fpu_exec.rs` (142 sites, disjoint opcodes -- they
//! simply under-charge under epoch 2), and, importantly, the JIT's
//! `DirectKind::raw_clocks`, which mirrors the SAME opcodes `execute.rs` serves.
//! So under epoch 2 a natively compiled block and an interpreted one charge
//! DIFFERENT numbers for the same instruction. That is fixed by slice 1 item 2
//! (the class index in the slot), which is the next sub-slice; until it lands,
//! `IZARRAVM_TIMING_EPOCH=2` is a development knob and no epoch-2 rate is a
//! measurement.
//!
//! Epoch 1 is unaffected by all of it: every routed site charges the literal it
//! carried before, which is what the tree's several hundred exact-clock
//! assertions check.
//!
//! # What this slice does NOT do
//!
//! The slot-index migration, the budget path, the four width sites, the class
//! histogram and the Dhrystone reconciliation are items 2-6 of slice 1 and are
//! later sub-slices. Nothing here changes a charge under epoch 1.

// `name`, `provenance`, `ALL` and `N_CLASSES` are consumed by the tests today and
// by the class histogram (design section 9.1) when it lands in a later sub-slice;
// the classes for the sites this slice has not routed yet are likewise
// constructed only from `ALL`. A plain build therefore sees them as unused, and
// `-D warnings` would refuse a table that is correct and simply not wired up yet.
#![allow(dead_code)]

use izarravm_core::CpuPersona;

/// Declare the class enum, its dense index, and the three persona columns from
/// ONE list, so a new class cannot be added to the enum and forgotten in a
/// table (the failure mode `DirectKind::raw_clocks`'s `_ => 2` default has
/// shipped twice this campaign).
///
/// Columns are `(epoch-1 literal, I486 epoch 2, I586 epoch 2, provenance)`.
/// There is no I386 epoch-2 column on purpose: the 386 is out of the
/// recalibration's scope (design §9.9), so `class_table(I386, _)` is [`EPOCH1`]
/// under both epochs and the 386 stays byte-identical forever.
macro_rules! timing_classes {
    ($( $(#[$meta:meta])* $name:ident = ($e1:expr, $i486:expr, $i586:expr, $prov:expr) ),+ $(,)?) => {
        /// One variant per distinct charge shape the decoder produces.
        ///
        /// "Distinct" is decided by the pair (semantic family, epoch-1
        /// literal): two sites that charge the same literal today but are
        /// different instructions on a real part -- `PUSH r` at 2 and `LEA` at
        /// 2, say -- are different classes, because epoch 2 must be free to
        /// separate them. Two sites that are the same instruction at two
        /// operand widths and charge the same on both references share one.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub(crate) enum TimingClass {
            $( $(#[$meta])* $name, )+
            /// The escape hatch: a charge site not yet classified, carrying its
            /// own epoch-1 literal.
            ///
            /// It charges that literal unchanged under EVERY epoch and persona,
            /// which is correct for epoch 1 and a recorded under-charge for
            /// epoch 2. It exists so routing can proceed site by site instead
            /// of as one 274-site commit; a `Legacy` site is not a table row and
            /// is deliberately invisible to [`TimingClass::ALL`]. Later
            /// sub-slices empty it out.
            Legacy(u16),
        }

        /// The dense ordinals, as a FIELDLESS twin of `TimingClass`.
        ///
        /// It exists only so `TimingClass::index` can be a `match` onto
        /// compile-time constants rather than a linear scan: `TimingClass`
        /// carries a payload in `Legacy`, so `self as usize` is not available
        /// on it, and `index` is called once per retired instruction. A match
        /// onto constants lowers to a single byte-table load; a scan over 138
        /// `matches!` would not.
        #[repr(u16)]
        #[allow(dead_code)]
        enum ClassOrdinal { $( $name, )+ }

        impl TimingClass {
            /// Every table-backed class, in table order. `Legacy` is absent by
            /// construction -- it has no table row.
            pub(crate) const ALL: &'static [TimingClass] = &[ $( TimingClass::$name, )+ ];

            /// This class's dense index into a [`ClassTable`].
            ///
            /// # Panics
            /// On [`TimingClass::Legacy`], which has no table row. Callers go
            /// through [`ClassTable::raw`], which handles it.
            pub(crate) const fn index(self) -> usize {
                match self {
                    $( TimingClass::$name => ClassOrdinal::$name as usize, )+
                    TimingClass::Legacy(_) => panic!(
                        "TimingClass::Legacy carries its own literal and has no table index"
                    ),
                }
            }

            /// The class at a dense index, the inverse of [`TimingClass::index`].
            ///
            /// The JIT's `Load`/`LoadExtend`/`Store` slots store their class as a
            /// `u8` INDEX where they used to store a raw clock count -- the same
            /// byte, so the slot does not widen (review B4) -- and this is how a
            /// slot turns back into a class at charge time.
            ///
            /// # Panics
            /// On an index no class owns. That can only happen if a slot's byte
            /// was built from something other than `index()`, which is a bug in
            /// the emitter rather than a runtime condition.
            /// One indexed load out of `ALL`, which `class_indices_are_a_dense_
            /// permutation` and `epoch_one_charges_the_pinned_literal_for_every_
            /// class` together pin to be in table order.
            #[inline]
            pub(crate) fn from_index(index: u8) -> TimingClass {
                TimingClass::ALL[index as usize]
            }

            /// The variant's name, for test failure messages and the future
            /// class histogram (design §9.1).
            pub(crate) const fn name(self) -> &'static str {
                match self {
                    $( TimingClass::$name => stringify!($name), )+
                    TimingClass::Legacy(_) => "Legacy",
                }
            }

            /// Where this class's epoch-2 numbers came from. See the module
            /// docs for the three spellings.
            pub(crate) const fn provenance(self) -> &'static str {
                match self {
                    $( TimingClass::$name => $prov, )+
                    TimingClass::Legacy(_) => "unclassified site; charges its epoch-1 literal",
                }
            }
        }

        /// How many table-backed classes exist.
        pub(crate) const N_CLASSES: usize = TimingClass::ALL.len();

        /// The epoch-1 column: today's literal for every class, for every
        /// persona. This array IS the byte-identity proof -- see the module docs.
        pub(crate) static EPOCH1: ClassTable = ClassTable([ $( $e1, )+ ]);

        /// The I486 epoch-2 column (`dev_docs/2026-09-05-486-timing-audit.md` §5).
        pub(crate) static EPOCH2_I486: ClassTable = ClassTable([ $( $i486, )+ ]);

        /// The I586 epoch-2 column (design §3.2 as amended by §9).
        pub(crate) static EPOCH2_I586: ClassTable = ClassTable([ $( $i586, )+ ]);
    };
}

// ---------------------------------------------------------------------------
// The class list.
//
// Ordering is by family, not by value: data movement, stack, control transfer,
// flags, shifts and the group-3 arm, the segment/system rows, strings, port
// I/O, and finally x87. Each row's comment names the interpreter site it was
// derived from, so the routing commit that follows can be checked against it.
// ---------------------------------------------------------------------------
timing_classes! {
    // --- register-resident ALU and data movement -----------------------------
    /// Register-to-register ALU / `CMP` / `TEST` / `MOV` / `INC` / `DEC`.
    /// `execute.rs` `execute_alu_decoded`, the `0x88`..`0x8b` register forms,
    /// `0x84`/`0x85`, `run.rs`'s `0x40..=0x4f`.
    Reg = (2, 12, 12, "cmp §3 row 1 (1 clk, UV)"),
    /// ALU with a memory SOURCE (`ADD r, m`) -- Intel's 2-clock load form.
    AluRegMem = (2, 24, 24, "cmp §3 row 3 (2 clk, UV)"),
    /// ALU with a memory DESTINATION (`ADD m, r`) -- the 3-clock read/modify/write.
    AluMemReg = (2, 36, 36, "cmp §3 row 4 (3 clk, UV)"),
    /// `MOV r, m`. One clock on both references; today's 2 is the i386 column.
    MovRegMem = (2, 12, 12, "cmp §3 row 2 (1 clk, UV)"),
    /// `MOV m, r`.
    MovMemReg = (2, 12, 12, "cmp §3 row 2 (1 clk, UV)"),
    /// `MOV r, imm` (`0xb0`..`0xbf`) and `MOV r/m, imm` register form.
    MovImmReg = (2, 12, 12, "cmp §3 row 1 (MOV, 1 clk)"),
    /// `MOV m, imm` (`0xc6`/`0xc7` memory form).
    MovImmMem = (2, 12, 12, "cmp §3 row 2 (1 clk)"),
    /// `MOV AL/eAX, moffs` and back (`0xa0`..`0xa3`) -- a `MOV r,m` / `m,r` in
    /// Intel's table; today's 4 is the i386's moffs penalty.
    MovAccMoffs = (4, 12, 12, "cmp §3 row 2 (1 clk)"),
    /// `LEA` (`0x8d`) -- address arithmetic, no memory access.
    Lea = (2, 12, 12, "cmp §3 row 1 (LEA, 1 clk)"),
    /// `XCHG` in every form (`0x86`, `0x87`, `0x91..=0x97`), today's
    /// `XCHG_CORE_CLOCKS`.
    Xchg = (3, 36, 36, "UNSOURCED x12"),
    /// `MOVZX`/`MOVSX` (`0x0fb6`..`0x0fbf`), both widths, both operand forms.
    MovExtend = (3, 36, 36, "UNSOURCED x12"),

    // --- flag and accumulator housekeeping -----------------------------------
    /// `CLC`/`STC`/`CMC`/`CLD`/`STD`/`SALC` -- the one-byte flag writes.
    FlagOp = (2, 24, 24, "UNSOURCED x12"),
    /// `CLI` (`0xfa`), today's `CLI_CORE_CLOCKS`.
    Cli = (3, 36, 36, "UNSOURCED x12"),
    /// `STI` (`0xfb`), today's `STI_CORE_CLOCKS`. A separate class from `Cli`
    /// for the reason the two constants are separate: separate interpreter arms.
    Sti = (3, 36, 36, "UNSOURCED x12"),
    /// `SAHF` (`0x9e`).
    Sahf = (3, 36, 36, "UNSOURCED x12"),
    /// `LAHF` (`0x9f`).
    Lahf = (2, 24, 24, "UNSOURCED x12"),
    /// `CBW`/`CWDE` (`0x98`).
    Cbw = (3, 36, 36, "UNSOURCED x12"),
    /// `CWD`/`CDQ` (`0x99`).
    Cwd = (2, 24, 24, "UNSOURCED x12"),
    /// `DAA`/`DAS`/`AAA`/`AAS` (`0x27`/`0x2f`/`0x37`/`0x3f`).
    DecimalAdjust = (4, 48, 48, "UNSOURCED x12"),
    /// `AAM` (`0xd4`).
    Aam = (17, 204, 204, "UNSOURCED x12"),
    /// `AAD` (`0xd5`).
    Aad = (19, 228, 228, "UNSOURCED x12"),
    /// `XLAT` (`0xd7`).
    Xlat = (5, 60, 60, "UNSOURCED x12"),

    // --- stack ---------------------------------------------------------------
    /// `PUSH r` (`0x50..=0x57`).
    PushReg = (2, 12, 12, "cmp §3 row 1 / 486 §5 (1 clk)"),
    /// `POP r` (`0x58..=0x5f`).
    PopReg = (4, 12, 12, "cmp §3 row 1 / 486 §5 (1 clk)"),
    /// `PUSH imm8`/`imm32` (`0x68`, `0x6a`).
    PushImm = (2, 12, 12, "cmp §3 row 1 (1 clk)"),
    /// `PUSH r/m` memory form (`0xff /6`), today's `PUSH_RM_CORE_CLOCKS`.
    PushMem = (2, 48, 24, "486 §5 (4 clk) / cmp §3 row 5 (2 clk, NP)"),
    /// `POP r/m` memory form (`0x8f`), today's `POP_RM_CORE_CLOCKS`.
    PopMem = (5, 60, 36, "486 §5 (5 clk) / cmp §3 row 5 (3 clk, NP)"),
    /// `PUSH Sreg` (`0x06`/`0x0e`/`0x16`/`0x1e`, `0x0fa0`, `0x0fa8`).
    PushSeg = (2, 36, 12, "486 §5 (3 clk) / cmp §3 row 1 (1 clk)"),
    /// `POP Sreg` (`0x07`/`0x1f`, `0x0fa1`, `0x0fa9`).
    PopSeg = (7, 36, 24, "486 §5 MovSregReg 36 real / cmp §3 row 15 (2 clk real)"),
    /// `POP SS` (`0x17`) -- today's `POP_SS_CORE_CLOCKS`, a separate arm from
    /// `PopSeg` because it arms the SS interrupt shadow.
    PopSs = (7, 36, 24, "486 §5 MovSregReg 36 real / cmp §3 row 15 (2 clk real)"),
    /// `PUSHA`/`PUSHAD` (`0x60`), today's `PUSH_ALL_CORE_CLOCKS`.
    PushAll = (18, 132, 60, "486 §5 (11 clk) / Intel App. F PUSHA 5 clk"),
    /// `POPA`/`POPAD` (`0x61`), today's `POP_ALL_CORE_CLOCKS`.
    PopAll = (18, 108, 60, "486 §5 (9 clk) / Intel App. F POPA 5 clk"),
    /// `PUSHF`/`PUSHFD` (`0x9c`), today's `PUSHF_CORE_CLOCKS`.
    PushFlags = (3, 48, 36, "UNSOURCED x12 (486 4 clk assumed)"),
    /// `POPF`/`POPFD` (`0x9d`), today's `POPF_CORE_CLOCKS`.
    PopFlags = (4, 108, 72, "UNSOURCED x12 (486 9 / P5 6 clk assumed)"),
    /// `ENTER` (`0xc8`).
    Enter = (10, 168, 132, "UNSOURCED x12 (486 14 / P5 11 clk assumed)"),
    /// `LEAVE` (`0xc9`), both operand sizes and both stack widths.
    Leave = (4, 60, 36, "UNSOURCED x12 (486 5 / P5 3 clk assumed)"),

    // --- control transfer ----------------------------------------------------
    /// `Jcc` short and near, taken or not.
    ///
    /// The interpreter arm charges ONE number past both branches, so this site
    /// cannot tell taken from not-taken. The I586 entry is design §3.4's
    /// blended 16 (1.33 clk). The I486 entry takes the audit's **taken** 36
    /// rather than its 12/36 split, deliberately: the exit-edge split the audit
    /// says the 486 could afford is a later sub-slice, and until it lands the
    /// 12:15 ruling says take the slower charge.
    Jcc = (3, 36, 16, "486 §5 (taken 3 clk; the 12/36 split is deferred) / design §3.4 blend"),
    /// Near `CALL`/`JMP` with a relative displacement (`0xe8`/`0xe9`/`0xeb`).
    CallJmpRel = (7, 36, 12, "486 §5 (3 clk) / cmp §3 row 6 (1 clk, PV)"),
    /// Near `CALL`/`JMP` through a register or memory (`0xff /2`, `/4`).
    CallJmpRm = (7, 60, 24, "486 §5 (5 clk) / cmp §3 row 7 (2 clk, NP)"),
    /// `RET` near, no immediate (`0xc3`).
    RetNear = (10, 60, 24, "486 §5 (5 clk) / cmp §3 row 7 (2 clk, NP)"),
    /// `RET` near with an immediate stack adjust (`0xc2`).
    RetNearImm = (10, 60, 36, "486 §5 (5 clk) / cmp §3 row 7 (3 clk, NP)"),
    /// `CALL` far (`0x9a`, and `0xff /3` through memory).
    CallFar = (17, 204, 204, "UNSOURCED x12"),
    /// `JMP` far (`0xea`, and `0xff /5` through memory).
    JmpFar = (17, 204, 204, "UNSOURCED x12"),
    /// `RETF` (`0xca`/`0xcb`), both operand sizes.
    RetFar = (17, 204, 204, "UNSOURCED x12"),
    /// Far `CALL` (`0xff /3`) and far `JMP` (`0xff /5`) THROUGH MEMORY, which
    /// charge 11 where the direct `0x9a`/`0xea` forms charge 17. A separate
    /// class rather than a shared one because the two literals differ today, and
    /// a class may hold only one epoch-1 value.
    CallJmpFarMem = (11, 132, 132, "UNSOURCED x12"),
    /// `LOOP` (`0xe2`) -- charged taken or not, one arm past both branches.
    Loop = (11, 84, 66, "486 §5 (7 clk taken) / design §3.2 (5.5 clk)"),
    /// `LOOPE`/`LOOPNE` (`0xe0`/`0xe1`).
    LoopCc = (11, 108, 90, "486 §5 (9 clk taken) / design §3.2 (7.5 clk)"),
    /// `JCXZ`/`JECXZ` (`0xe3`).
    Jcxz = (9, 96, 66, "486 §5 (8 clk taken) / design §3.2 (5.5 clk)"),
    /// `NOP` (`0x90`), which is `XCHG eAX, eAX` and charges its own number.
    Nop = (3, 12, 12, "cmp §3 row 1 (1 clk)"),
    /// `INT imm8` (`0xcd`), today's `INT_IMM8_CORE_CLOCKS`.
    ///
    /// Real mode. The V86 row (design §3.2: 720) needs the mode at the charge
    /// site and is a later sub-slice; charging the real-mode number in V86 is
    /// an under-charge and is recorded here rather than guessed at.
    IntN = (37, 360, 204, "486 §5 (30 clk real) / cmp §3 row 17 (17 clk real)"),
    /// `INT 3` (`0xcc`).
    Int3 = (33, 360, 204, "486 §5 IntN / cmp §3 row 17 (INT n family)"),
    /// `INTO` (`0xce`) when the overflow flag is set.
    IntO = (35, 360, 204, "486 §5 IntN / cmp §3 row 17 (INT n family)"),
    /// `INTO` (`0xce`) when it falls through.
    IntONotTaken = (3, 36, 36, "UNSOURCED x12"),
    /// `IRET`/`IRETD` (`0xcf`).
    Iret = (22, 180, 84, "486 §5 (15 clk) / cmp §3 row 17 (7 clk real)"),

    // --- shifts, rotates, and the group-3 arm --------------------------------
    /// Shift/rotate by an immediate count OR by 1 (`0xc0`/`0xc1`/`0xd0`/`0xd1`).
    ///
    /// The two encodings are ONE class deliberately, and the reason is the JIT.
    /// `0xd1` and `0xc1` with an immediate of 1 produce the **same**
    /// `DirectKind::Shift` (`jit/direct.rs`'s own note beside the `0xc1 | 0xd1`
    /// arm), and `DirectInsn` carries no opcode, so the compiled block cannot
    /// tell them apart at charge time. The 486 prices them differently (2 vs 3
    /// clocks) and the 586 does not (1 clock either way), so keeping them apart
    /// would buy one clock on one persona at the cost of native and interpreted
    /// code charging differently for the same instruction -- the exact
    /// divergence the arm-equality bar exists to stop. The shared entry takes the
    /// SLOWER of the two 486 counts, per the owner's 12:15 ruling.
    ShiftImm = (2, 36, 12, "486 §5 (3 clk, the slower of 2/3) / cmp §3 row 11 (1 clk, PU)"),
    /// Shift/rotate by `CL` (`0xd2`/`0xd3`).
    ///
    /// `RCL`/`RCR` by `CL` is a much more expensive instruction on both parts
    /// (486 8-30, P5 7-24) and wants its own class; the interpreter's group-2
    /// arm serves all eight sub-opcodes from one `Ok(clocks(2))`, so splitting
    /// it needs the sub-opcode at the charge site. Deferred with `Group3`, and
    /// the shared class takes the cheaper shift number, an under-charge.
    ShiftCl = (2, 36, 48, "486 §5 (3 clk) / cmp §3 row 11 (4 clk, NP)"),
    /// `SHLD`/`SHRD` (`0x0fa4`/`0x0fa5`/`0x0fac`/`0x0fad`).
    DoubleShift = (3, 36, 48, "UNSOURCED x12 (486 3 / P5 4 clk assumed)"),
    // --- group 3 (`0xf6`/`0xf7`), SPLIT -------------------------------------
    // Design §9.1's headline finding: one `clocks(2)` used to serve all seven
    // sub-opcodes at every width, giving `DIV EAX, ECX` the cost of
    // `MOV EAX, EBX` -- 0.167 guest clocks against Intel's 41, a **246x**
    // under-charge. The interpreter splits on `modrm.reg` plus the operand
    // shape; the JIT splits on the kind, since `classify` already produces
    // separate `MulReg`/`MulMemAcc`/`ImulRegAcc`/`ImulMemAcc`/`DivReg`/`DivMem`/
    // `NegReg`/`TestImmReg`/`TestImmMem` kinds and admits them at Dword only.
    /// `TEST r, imm` -- group 3 `/0` register form, and `0xa8`/`0xa9`.
    TestImmReg = (2, 12, 12, "cmp §3 row 1 (1 clk)"),
    /// `TEST m, imm` -- group 3 `/0` memory form, a load rather than an RMW.
    TestImmMem = (2, 24, 24, "cmp §3 row 3 (2 clk load)"),
    /// `NOT`/`NEG r` -- group 3 `/2`, `/3` register form.
    NotNegReg = (2, 12, 12, "cmp §3 row 1 (1 clk)"),
    /// `NOT`/`NEG m` -- group 3 `/2`, `/3` memory form, a read/modify/write.
    NotNegMem = (2, 36, 36, "cmp §3 row 4 (3 clk RMW)"),
    /// `MUL`/`IMUL r/m8` -- group 3 `/4`, `/5` at byte width. One class for both
    /// sub-opcodes because both references price them together (comparison
    /// §3 row 13; audit §5 `Mul` 8/16/32).
    Mul8 = (2, 186, 132, "486 §5 (13-18, midpoint 15.5) / cmp §3 row 13 (11 clk)"),
    /// `MUL`/`IMUL r/m16`.
    Mul16 = (2, 234, 132, "486 §5 (13-26, midpoint 19.5) / cmp §3 row 13 (11 clk)"),
    /// `MUL`/`IMUL r/m32`.
    Mul32 = (2, 330, 120, "486 §5 (13-42, midpoint 27.5) / cmp §3 row 13 (10 clk)"),
    /// `DIV r/m8` -- group 3 `/6` at byte width.
    Div8 = (2, 192, 204, "486 §5 (16 clk) / cmp §3 row 14 (17 clk)"),
    /// `DIV r/m16`.
    Div16 = (2, 288, 300, "486 §5 (24 clk) / cmp §3 row 14 (25 clk)"),
    /// `DIV r/m32` -- the 246x row itself.
    Div32 = (2, 480, 492, "486 §5 (40 clk) / cmp §3 row 14 (41 clk)"),
    /// `IDIV r/m8` -- group 3 `/7` at byte width.
    Idiv8 = (2, 228, 264, "486 §5 (19 clk) / cmp §3 row 14 (22 clk)"),
    /// `IDIV r/m16`.
    Idiv16 = (2, 324, 360, "486 §5 (27 clk) / cmp §3 row 14 (30 clk)"),
    /// `IDIV r/m32`.
    Idiv32 = (2, 516, 552, "486 §5 (43 clk) / cmp §3 row 14 (46 clk)"),
    /// `INC`/`DEC r/m8` (`0xfe`), today's `INC_DEC_RM8_CORE_CLOCKS`. The memory
    /// form is a read/modify/write, so it takes `AluMemReg`'s shape.
    IncDecRm = (2, 36, 36, "cmp §3 row 4 (3 clk RMW)"),
    /// Two-operand `IMUL r, r/m` (`0x0faf`), both operand forms.
    ImulRm = (9, 234, 120, "486 §5 Mul 16-bit midpoint / cmp §3 row 13 (10 clk r/m32)"),
    /// Three-operand `IMUL r, r/m, imm` (`0x69`/`0x6b`).
    ImulImm = (14, 234, 132, "486 §5 Mul midpoint / cmp §3 row 13 (11 clk)"),

    // --- bit operations ------------------------------------------------------
    /// `BT`/`BTS`/`BTR`/`BTC` in both encodings, today's `BIT_STRING_CORE_CLOCKS`.
    BitTest = (6, 72, 72, "UNSOURCED x12"),
    /// `BSF`/`BSR` (`0x0fbc`/`0x0fbd`).
    BitScan = (10, 120, 120, "UNSOURCED x12"),
    /// `BSWAP` (`0x0fc8..=0x0fcf`).
    Bswap = (1, 12, 12, "UNSOURCED x12"),
    /// `CMPXCHG` (`0x0fb0`/`0x0fb1`).
    CmpXchg = (6, 72, 72, "UNSOURCED x12"),
    /// `CMPXCHG8B` (`0x0fc7`).
    CmpXchg8b = (10, 120, 120, "UNSOURCED x12"),
    /// `XADD` (`0x0fc0`/`0x0fc1`).
    Xadd = (4, 48, 48, "UNSOURCED x12"),
    /// `SETcc` (`0x0f90..=0x0f9f`), register and memory forms alike.
    SetCc = (4, 48, 48, "UNSOURCED x12"),

    // --- segment and system --------------------------------------------------
    /// `MOV r/m16, Sreg` (`0x8c`), today's `MOV_RM_SREG_CORE_CLOCKS`.
    MovRegSreg = (2, 36, 12, "486 §5 (3 clk) / cmp §3 row 15 (1 clk)"),
    /// `MOV Sreg, r/m` (`0x8e`), today's `MOV_SREG_CORE_CLOCKS`. Real mode; the
    /// protected-mode row (486 108, P5 132 + 96 per unaccessed descriptor)
    /// needs the mode at the charge site and is a later sub-slice.
    MovSregReg = (7, 36, 24, "486 §5 (3 clk real) / cmp §3 row 15 (2 clk real)"),
    /// `LES`/`LDS` (`0xc4`/`0xc5`) and `LFS`/`LGS`/`LSS` (`0x0fb2`..`0x0fb5`).
    LesLds = (7, 84, 84, "UNSOURCED x12"),
    /// `LAR` (`0x0f02`).
    Lar = (11, 132, 132, "UNSOURCED x12"),
    /// `LSL` (`0x0f03`).
    Lsl = (11, 132, 132, "UNSOURCED x12"),
    /// `VERR`/`VERW` (`0x0f00 /4`, `/5`).
    VerRw = (10, 120, 120, "UNSOURCED x12"),
    /// `SLDT`/`STR` (`0x0f00 /0`, `/1`).
    SldtStr = (2, 24, 24, "UNSOURCED x12"),
    /// `LLDT`/`LTR` (`0x0f00 /2`, `/3`).
    LldtLtr = (11, 132, 132, "UNSOURCED x12"),
    /// `SGDT`/`SIDT` (`0x0f01 /0`, `/1`), memory form only.
    SgdtSidt = (11, 132, 132, "UNSOURCED x12"),
    /// `LGDT`/`LIDT` (`0x0f01 /2`, `/3`).
    LgdtLidt = (11, 132, 132, "UNSOURCED x12"),
    /// `SMSW` (`0x0f01 /4`).
    Smsw = (2, 24, 24, "UNSOURCED x12"),
    /// `LMSW` (`0x0f01 /6`).
    Lmsw = (3, 36, 36, "UNSOURCED x12"),
    /// `INVLPG` (`0x0f01 /7`).
    Invlpg = (12, 144, 144, "UNSOURCED x12"),
    /// `CLTS` (`0x0f06`).
    Clts = (2, 24, 24, "UNSOURCED x12"),
    /// `MOV` to and from `CRn`/`DRn` (`0x0f20`..`0x0f23`).
    MovCrDr = (6, 72, 72, "UNSOURCED x12"),
    /// `BOUND` (`0x62`).
    Bound = (10, 120, 120, "UNSOURCED x12"),
    /// `WRMSR` (`0x0f30`).
    Wrmsr = (30, 360, 360, "UNSOURCED x12"),
    /// `RDTSC` (`0x0f31`).
    Rdtsc = (11, 132, 132, "UNSOURCED x12"),
    /// `RDMSR` (`0x0f32`).
    Rdmsr = (11, 132, 132, "UNSOURCED x12"),
    /// `INVD`/`WBINVD` (`0x0f08`/`0x0f09`).
    InvdWbinvd = (4, 48, 48, "UNSOURCED x12"),
    /// `CPUID` (`0x0fa2`).
    Cpuid = (14, 168, 168, "UNSOURCED x12"),

    // --- strings and port I/O -----------------------------------------------
    /// One string instruction (`MOVS`/`STOS`/`LODS`/`CMPS`/`SCAS`), today's flat
    /// `STRING_CORE_CLOCKS` for the whole burst.
    ///
    /// Design §3.2 splits this into a per-element cost plus a setup cost, per
    /// family; that split is `RepLimitPlan::compute`'s problem (slice 1, item 5)
    /// and is a later sub-slice, so this stays one flat class.
    StringElem = (4, 48, 48, "PLACEHOLDER x12 -- the per-element split is a later sub-slice"),
    /// `INS`/`INSB`/`INSW`/`INSD` (`0x6c`/`0x6d`).
    InsString = (15, 180, 180, "UNSOURCED x12 -- owned by the port slice"),
    /// `OUTS`/`OUTSB`/`OUTSW`/`OUTSD` (`0x6e`/`0x6f`).
    OutsString = (14, 168, 168, "UNSOURCED x12 -- owned by the port slice"),
    /// `IN AL/eAX, imm8` and `IN AL/eAX, DX`, today's `IN_PORT_CORE_CLOCKS`.
    ///
    /// P1 (`dev_docs/2026-09-05-port-io-repricing-design.md`) owns the epoch-2
    /// port charge and applies it on the BUS side, not here; this row keeps the
    /// core term so a port access does not lose its core cost when the bus term
    /// moves. 486 §5's 168/192 are recorded, not yet reconciled with P1.
    InPort = (12, 168, 168, "486 §5 (14 clk) -- the bus term is P1's"),
    /// `OUT imm8/DX, AL/eAX`, today's `OUT_PORT_CORE_CLOCKS`.
    OutPort = (10, 192, 192, "486 §5 (16 clk) -- the bus term is P1's"),
    /// `IN eAX, imm8`/`IN eAX, DX` at the dword width, which charge 12 from a
    /// literal rather than through `IN_PORT_CORE_CLOCKS`.
    InPortDword = (12, 168, 168, "486 §5 (14 clk) -- the bus term is P1's"),

    // --- x87 -----------------------------------------------------------------
    // Every x87 charge is scaled a second time by `fp_timing_class`, which is a
    // SEPARATE dial and is untouched here (design §4 deletes its `IntConvert32`
    // absorber in slice 4, not now).
    /// `WAIT`/`FWAIT` (`0x9b`) with no pending unmasked exception.
    X87Wait = (6, 72, 72, "UNSOURCED x12"),
    /// `FADD`/`FSUB`/`FMUL`/`FDIV`/`FCOM` with an m32 real operand (`0xd8`).
    X87MemArith32 = (20, 240, 36, "cmp §3 row 19 (FADD 3 clk latency)"),
    /// The same family with an m64 real operand (`0xdc`).
    X87MemArith64 = (20, 240, 36, "cmp §3 row 19 (FADD 3 clk latency)"),
    /// `FIADD`/`FIMUL`/... with an m32 integer operand (`0xda`).
    X87MemArithInt32 = (20, 240, 240, "UNSOURCED x12"),
    /// The same family with an m16 integer operand (`0xde`).
    X87MemArithInt16 = (20, 240, 240, "UNSOURCED x12"),
    /// `FLD m32` (`0xd9 /0`).
    X87LoadReal32 = (14, 168, 12, "cmp §3 row 19 (FLD m32 1 clk)"),
    /// `FST`/`FSTP m32` (`0xd9 /2`, `/3`).
    X87StoreReal32 = (14, 168, 24, "cmp §3 row 19 (FST m32 2 clk)"),
    /// `FLD m64` (`0xdd /0`).
    X87LoadReal64 = (14, 168, 12, "cmp §3 row 19 (FLD m64 1 clk)"),
    /// `FST`/`FSTP m64` (`0xdd /2`, `/3`).
    X87StoreReal64 = (14, 168, 24, "cmp §3 row 19 (FST 2 clk)"),
    /// `FLD m80` (`0xdb /5`).
    X87LoadExtended80 = (14, 168, 168, "UNSOURCED x12"),
    /// `FSTP m80` (`0xdb /7`).
    X87StoreExtended80 = (14, 168, 168, "UNSOURCED x12"),
    /// `FILD m32` (`0xdb /0`). Intel's row is `3/1` (latency/throughput); the
    /// 12:15 ruling takes the slow side.
    X87LoadInt32 = (14, 168, 36, "cmp §3 row 20 (FILD m32 3 clk latency, slow side)"),
    /// **`FIST`/`FISTP m32`** (`0xdb /2`, `/3`) -- the fixed-point boundary the
    /// Quake span rasterizer lives on, and the row `fp_timing_class`'s x34
    /// absorber was fitted around.
    X87StoreInt32 = (14, 168, 72, "cmp §3 row 20 (FISTP m32 6 clk)"),
    /// `FILD m16` (`0xdf /0`).
    X87LoadInt16 = (14, 168, 36, "cmp §3 row 20 (FILD family, slow side)"),
    /// `FIST`/`FISTP m16` (`0xdf /2`, `/3`).
    X87StoreInt16 = (14, 168, 72, "cmp §3 row 20 (FISTP family)"),
    /// `FILD m64` (`0xdf /5`).
    X87LoadInt64 = (14, 168, 36, "cmp §3 row 20 (FILD family, slow side)"),
    /// `FISTP m64` (`0xdf /7`).
    X87StoreInt64 = (14, 168, 72, "cmp §3 row 20 (FISTP family)"),
    /// `FLDCW` (`0xd9 /5`).
    X87LoadControl = (4, 48, 48, "UNSOURCED x12"),
    /// `FNSTCW` (`0xd9 /7`).
    X87StoreControl = (14, 168, 168, "UNSOURCED x12"),
    /// `FNSTSW m16` (`0xdd /7`).
    X87StoreStatus = (14, 168, 168, "UNSOURCED x12"),
    /// `FLDENV` (`0xd9 /4`).
    X87LoadEnv = (44, 528, 528, "UNSOURCED x12"),
    /// `FNSTENV` (`0xd9 /6`).
    X87StoreEnv = (56, 672, 672, "UNSOURCED x12"),
    /// `FRSTOR` (`0xdd /4`).
    X87Restore = (75, 900, 900, "UNSOURCED x12"),
    /// `FNSAVE` (`0xdd /6`).
    X87Save = (150, 1800, 1800, "UNSOURCED x12"),
    /// `FBLD` (`0xdf /4`).
    X87LoadBcd = (75, 900, 900, "UNSOURCED x12"),
    /// `FBSTP` (`0xdf /6`).
    X87StoreBcd = (160, 1920, 1920, "UNSOURCED x12"),
    /// Register-form `FADD`/`FSUB`/`FMUL`/`FDIV` and their `P`/`R` variants.
    ///
    /// One class for the whole arithmetic family, which is what the interpreter
    /// site gives: `fpu_reg_arith_st0`/`_sti` charge one number for all of them.
    /// Design §3.2 wants `FDIV` at 468 and `FSQRT` at 840 separately, and R7
    /// makes the `FDIV` ladder slice 4's problem; splitting the family needs the
    /// sub-opcode at the charge site and is a later sub-slice. This entry takes
    /// `FADD`'s 3-clock latency, an under-charge for `FDIV` recorded here.
    X87RegArith = (20, 240, 36, "cmp §3 row 19 (FADD 3 clk); FDIV/FSQRT split deferred"),
    /// `FUCOM`/`FUCOMP` (`0xdd /4`, `/5`).
    X87RegCompare = (4, 48, 48, "UNSOURCED x12"),
    /// `FLD ST(i)` and `FXCH` (`0xd9 c0..cf`, `0xd9 d0`).
    X87RegExchange = (4, 48, 12, "design §3.2 (FXCH raw 12)"),
    /// `FCHS` (`0xd9 e0`) and `FABS` (`0xd9 e1`).
    X87RegSign = (6, 72, 72, "UNSOURCED x12"),
    /// `FXAM` (`0xd9 e5`) and the four-and-a-half-digit constant loads
    /// `FLDL2T`/`FLDL2E`/`FLDPI`/`FLDLG2`/`FLDLN2` (`0xd9 e9..ed`), which all
    /// charge 8 today. `FXAM` is not a constant load and shares the class only
    /// because it shares the literal; splitting it needs an epoch-2 source it
    /// does not have yet.
    X87RegConst = (8, 96, 96, "UNSOURCED x12"),
    /// `FTST` (`0xd9 e4`), `FLD1` (`0xd9 e8`) and `FLDZ` (`0xd9 ee`), the three
    /// register-form ops that charge 4 rather than 8 today.
    X87RegConstCheap = (4, 48, 48, "UNSOURCED x12"),
    /// `F2XM1` (`0xd9 f0`).
    X87Exp = (200, 2400, 2400, "UNSOURCED x12"),
    /// The transcendentals that charge 300: `FYL2X`, `FPTAN`, `FPATAN`,
    /// `FSIN`, `FCOS`, `FSINCOS`, `FYL2XP1` (`0xd9 f1..f3`, `f9`, `fb`, `fe`, `ff`).
    X87Transcendental = (300, 3600, 3600, "UNSOURCED x12"),
    /// `FXTRACT` (`0xd9 f4`) and `FSQRT` (`0xd9 fa`).
    ///
    /// Design §3.2 puts `FSQRT` at raw 840 on the 586; that number arrives with
    /// the slice-4 ladder that R7 requires, not here.
    X87Sqrt = (70, 840, 840, "design §3.2 (FSQRT 70 clk = raw 840)"),
    /// `FPREM` (`0xd9 f8`) and `FPREM1` (`0xd9 f5`).
    X87Rem = (100, 1200, 1200, "UNSOURCED x12"),
    /// `FRNDINT` (`0xd9 fc`).
    X87RoundInt = (20, 240, 240, "UNSOURCED x12"),
    /// `FSCALE` (`0xd9 fd`).
    X87Scale = (30, 360, 360, "UNSOURCED x12"),
    /// `FDECSTP`/`FINCSTP` (`0xd9 f6`/`0xd9 f7`).
    X87StackPointer = (4, 48, 48, "UNSOURCED x12"),
    /// `FNENI`/`FNDISI`/`FNSETPM` (`0xdb e0`/`e1`/`e4`, 387 no-ops) and `FNCLEX`
    /// (`0xdb e2`).
    X87Control = (2, 24, 24, "UNSOURCED x12"),
    /// `FNINIT` (`0xdb e3`).
    X87Init = (3, 36, 36, "UNSOURCED x12"),
    /// `FFREE ST(i)` (`0xdd /0`).
    X87Free = (3, 36, 36, "UNSOURCED x12"),
    /// `FNSTSW AX` (`0xdf e0`).
    X87StatusReg = (3, 36, 36, "UNSOURCED x12"),
    /// `FST ST(i)` / `FSTP ST(i)` register forms (`0xdd /2`, `/3`).
    X87RegStore = (3, 36, 36, "UNSOURCED x12"),
    /// `FCOMPP` (`0xde d9`) and `FUCOMPP` (`0xda e9`) -- compare and pop twice.
    X87ComparePop = (5, 60, 60, "UNSOURCED x12"),
}

/// One persona's charges, indexed by [`TimingClass::index`].
///
/// `u16` is deliberate and sufficient: the largest epoch-2 entry is
/// `X87Save`'s 1,800, and the design's worst named class (`IntN` in V86, 720)
/// is far below `u16::MAX`. The JIT's per-block sum is separately `u16` and
/// separately checked (review B4); this type does not widen it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClassTable([u16; N_CLASSES]);

impl ClassTable {
    /// The raw core clocks this table charges for `class`, before
    /// `level_timing`'s scaling.
    ///
    /// [`TimingClass::Legacy`] carries its own literal and bypasses the table
    /// entirely, which is what makes site-by-site routing safe.
    #[inline]
    pub(crate) const fn raw(&self, class: TimingClass) -> u32 {
        match class {
            TimingClass::Legacy(literal) => literal as u32,
            other => self.0[other.index()] as u32,
        }
    }

    /// The table as a slice, for the tests that iterate it.
    #[cfg(test)]
    pub(crate) const fn entries(&self) -> &[u16; N_CLASSES] {
        &self.0
    }
}

/// The charge table for one persona under one epoch, resolved ONCE at machine
/// construction (design §9.9; `dev_docs/2026-09-05-port-io-repricing-design.md`
/// §4: "the epoch may never change mid-run, because the JIT caches per-block raw
/// clocks").
///
/// Epoch 1 -- and I386 under every epoch -- is [`EPOCH1`], whose entries are the
/// literals the charge sites used to carry. That is the byte-identity proof: not
/// a test result, a property of the array.
///
/// An epoch above 2 resolves like epoch 2 rather than refusing: the knob parser
/// (`izarravm-machine`'s `parse_timing_epoch`) is the one place that rejects a
/// spelling, and duplicating the refusal here would put two answers in the tree.
///
/// The three tables are `static`, not `const`, so the returned reference has a
/// stable address: a `const` is inlined per use site, and two `&EPOCH1`s from
/// two call sites need not be the same pointer. The tests compare identity, and
/// the CPU caches the reference across a whole run.
pub(crate) fn class_table(persona: CpuPersona, epoch: u32) -> &'static ClassTable {
    if epoch < 2 {
        return &EPOCH1;
    }
    match persona {
        // The 386 is out of the recalibration's scope (design §9.9): no epoch-2
        // column exists for it and it stays byte-identical under both epochs.
        CpuPersona::I386 => &EPOCH1,
        CpuPersona::I486 => &EPOCH2_I486,
        CpuPersona::I586 => &EPOCH2_I586,
    }
}

#[cfg(test)]
#[path = "timing_class_test.rs"]
mod tests;
