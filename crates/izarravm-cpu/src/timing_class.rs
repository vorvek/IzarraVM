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
//! came from, and **every class is sourced** -- the `UNSOURCED x12` default the
//! table shipped with is gone, by the count asserted in
//! `the_unsourced_and_placeholder_census_is_pinned`. The spellings:
//!
//! * `F2 p.N` / `F3` / `F5` -- Intel's Pentium Tables F-2 (integer), F-3 (I/O)
//!   and F-5 (floating point), Appendix F of 241430-004. `INT` is the interrupt
//!   clock-count table.
//! * `T10.1` / `T10.2` / `T10.3` -- the i486 DX2 Data Book's Tables 10.1
//!   (integer), 10.2 (I/O) and 10.3 (floating point). The persona is a
//!   **DX2-66**, so the DX2 book is the primary 486 column wherever it differs
//!   from the base-i486 datasheet; each such row says so.
//! * `A1` / `3.6.2.1` -- the Optimization Manual's pairing table and rules.
//!   Table F-2's own `Pairing` column is column-drifted in the extraction and
//!   is never read positionally.
//! * `cmp §3 row N` -- `dev_docs/2026-09-05-86box-pentium-timing-comparison.md`
//!   §3, which is Appendix F read for the same rows.
//! * `486 §5` -- `dev_docs/2026-09-05-486-timing-audit.md` §5's I486 column.
//!
//! Conventions, all from `dev_docs/2026-09-05-class-table-sources.md`: a true
//! min/max range takes the SLOW end (the owner's 12:15 ruling -- "a miss on the
//! slow side is a soft finding; a miss on the fast side is a hard failure"); a
//! DATA-dependent MN/MX range takes a typical and records the range, following
//! `RclRcrCl`'s precedent; a mode split takes the real-mode row, as `IntN` and
//! `MovSregReg` already did; and an x87 latency/throughput pair takes the
//! LATENCY, which is what a non-overlapped retire actually costs and is the slow
//! side.
//!
//! **The I486 x87 column is the shipped literal times twelve, deliberately.**
//! `dev_docs/2026-09-05-k6-fpu-provenance.md` found that our x87 literals ARE
//! i486 DX2 Table 10.3 counts (FBLD 75, FLDENV 44, FSTENV 56, FCHS 6, FXCH 4,
//! FFREE 3, FADD 20 at the high end, FSAVE 150, with F2XM1 200 and the
//! transcendentals 300 as round stand-ins inside the 486's ranges), so the 486
//! arm was accidentally right all along and only the I586 column moved onto the
//! Pentium counts. The three classes SPLIT out during the sourcing
//! (`X87MemArithIntDiv32`/`16`, `X87Xam`) have no shipped literal of their own
//! and take Table 10.3 directly.
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
        pub(crate) static EPOCH1: ClassTable = EPOCH1_CONST;

        /// A `const` twin of [`EPOCH1`], for compile-time tripwires.
        ///
        /// Callers that need a stable ADDRESS take `&EPOCH1`; callers that need
        /// to evaluate at compile time take this, because a `const` may not read
        /// a `static`. The two are the same array by construction.
        pub(crate) const EPOCH1_CONST: ClassTable = ClassTable(EPOCH1_ENTRIES);

        /// `EPOCH1`'s array as a `const`, so the tripwire block below it can be
        /// evaluated at compile time: a `const` may not read a `static`, and
        /// `EPOCH1` has to be a `static` for its address to be stable.
        const EPOCH1_ENTRIES: [u16; N_CLASSES] = [ $( $e1, )+ ];

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
    Xchg = (3, 60, 36, "F2 p.F-22 (3, mem form) / T10.1 (5, mem form), slow end"),
    /// `MOVZX`/`MOVSX` (`0x0fb6`..`0x0fbf`), both widths, both operand forms.
    MovExtend = (3, 36, 36, "F2 (MOVSX/MOVZX 3) / T10.1 (3)"),

    // --- flag and accumulator housekeeping -----------------------------------
    /// `CLC`/`STC`/`CMC`/`CLD`/`STD`/`SALC` -- the one-byte flag writes.
    FlagOp = (2, 24, 24, "F2 (CLC/CLD/STC/STD/CMC 2) / T10.1 (2); SALC has no row on either part"),
    /// `CLI` (`0xfa`), today's `CLI_CORE_CLOCKS`.
    Cli = (3, 60, 84, "F2 (CLI 7) / T10.1 (CLI 5); V86 is INT+9, a later mode-keyed sub-slice"),
    /// `STI` (`0xfb`), today's `STI_CORE_CLOCKS`. A separate class from `Cli`
    /// for the reason the two constants are separate: separate interpreter arms.
    Sti = (3, 60, 84, "F2 (STI 7) / T10.1 (STI 5); V86 is INT+9, a later mode-keyed sub-slice"),
    /// `SAHF` (`0x9e`).
    Sahf = (3, 24, 24, "F2 (SAHF 2) / T10.1 (2)"),
    /// `LAHF` (`0x9f`).
    Lahf = (2, 36, 24, "F2 (LAHF 2) / T10.1 (3)"),
    /// `CBW`/`CWDE` (`0x98`).
    Cbw = (3, 36, 36, "F2 (CBW/CWDE 3) / T10.1 (3)"),
    /// `CWD`/`CDQ` (`0x99`).
    Cwd = (2, 36, 24, "F2 (CWD/CDQ 2) / T10.1 (3)"),
    /// `DAA`/`DAS`/`AAA`/`AAS` (`0x27`/`0x2f`/`0x37`/`0x3f`).
    DecimalAdjust = (4, 36, 36, "F2 (AAA/AAS/DAA/DAS 3) / T10.1 (AAA/AAS 3), slow end of the family"),
    /// `AAM` (`0xd4`).
    Aam = (17, 180, 216, "F2 (AAM 18) / T10.1 (AAM 15)"),
    /// `AAD` (`0xd5`).
    Aad = (19, 168, 120, "F2 (AAD 10) / T10.1 (AAD 14); the 486 is SLOWER than the P5 here"),
    /// `XLAT` (`0xd7`).
    Xlat = (5, 48, 48, "F2 (XLAT 4) / T10.1 (4)"),

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
    PushFlags = (3, 48, 36, "F2 (PUSHF 3 real) / T10.1 (PUSHF 4 real); V86 INT+9 deferred"),
    /// `POPF`/`POPFD` (`0x9d`), today's `POPF_CORE_CLOCKS`.
    PopFlags = (4, 108, 48, "F2 (POPF 4 real) / T10.1 (POPF 9 real)"),
    /// `ENTER` (`0xc8`).
    Enter = (10, 204, 180, "F2 (ENTER 15 at L>=1) / T10.1 (17); the +2L/+3L term wants a level-keyed site"),
    /// `LEAVE` (`0xc9`), both operand sizes and both stack widths.
    Leave = (4, 60, 36, "F2 (LEAVE 3) / T10.1 (5)"),

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
    CallFar = (17, 216, 60, "F2 (CALL far indirect 5 real) / T10.1 (18 real); protected 22-45+TS deferred"),
    /// `JMP` far (`0xea`, and `0xff /5` through memory).
    JmpFar = (17, 204, 48, "F2 (JMP far indirect 4 real) / T10.1 (17 real)"),
    /// `RETF` (`0xca`/`0xcb`), both operand sizes.
    RetFar = (17, 168, 48, "F2 (RETF 4 real) / T10.1 (14 real, imm form)"),
    /// Far `CALL` (`0xff /3`) and far `JMP` (`0xff /5`) THROUGH MEMORY, which
    /// charge 11 where the direct `0x9a`/`0xea` forms charge 17. A separate
    /// class rather than a shared one because the two literals differ today, and
    /// a class may hold only one epoch-1 value.
    /// Far `CALL`/`JMP` through memory take the same real-mode counts as their
    /// direct forms (`CallFar`/`JmpFar`); they are a separate class only because
    /// their epoch-1 literals differ (11 against 17), and a class holds one
    /// epoch-1 value. The epoch-2 entries take the slower of the two families,
    /// `CallFar`, per the 12:15 ruling.
    CallJmpFarMem = (11, 216, 60, "F2 (CALL far indirect 5 real) / T10.1 (18 real)"),
    /// A far `CALL`/`JMP`/`RETF` to a protected-mode CODE SEGMENT at the same
    /// privilege level -- no gate, no stack switch. The one flat 17 covered
    /// this and the three rows below it; the census measures the gate row at
    /// 15.5x under and the TSS row at ~122x.
    FarTransferPm = (17, 408, 264, "F2 (CALL far pm same level 22) / T10.1 (34 clk)"),
    /// A far transfer through a CALL GATE, which switches stacks and copies
    /// parameters.
    FarTransferGate = (17, 828, 528, "F2 (CALL gate different level 44) / T10.1 (69 clk)"),
    /// A far transfer through a TASK GATE or a TSS selector -- a full task
    /// switch, and the single largest missing term the census found.
    ///
    /// It is here AND `TaskSwitch` is charged on the switch path itself,
    /// because a task switch is also reachable from `IRET` with NT set and from
    /// an interrupt through a task gate, which never touch this class.
    FarTransferTss = (17, 2388, 2076, "F2 (CALL far to TSS 173) / T10.1 (199 clk)"),
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
    /// `INT imm8` taken from **V86** through a trap or interrupt gate to a
    /// different level -- the mode `IntN` used to serve from one flat 37.
    ///
    /// This is the row `dev_docs/2026-09-05-v86-port-io-timing-research.md`
    /// section 1.2 re-anchored: Intel's Interrupt Clock Counts Table gives the
    /// V86 / trap gate, different level entry as **54**, plus **12** on a cache
    /// miss, so 66 clocks and raw 792. The 486's own V86 gate row is 86.
    IntNV86 = (37, 1032, 792, "F2 INT table, V86/trap gate different level 54 +12 miss / 486 §5 (86 clk)"),
    /// `INT imm8` taken in protected mode to a different privilege level.
    IntNPm = (37, 828, 480, "F2 INT table, pm trap gate different level 40 / T10.1 (69 clk)"),
    /// `INT 3` (`0xcc`).
    Int3 = (33, 360, 204, "486 §5 IntN / cmp §3 row 17 (INT n family)"),
    /// `INTO` (`0xce`) when the overflow flag is set.
    IntO = (35, 360, 204, "486 §5 IntN / cmp §3 row 17 (INT n family)"),
    /// `INTO` (`0xce`) when it falls through.
    IntONotTaken = (3, 36, 48, "F2 (INTO not taken 4) / T10.1 (3)"),
    /// `IRET`/`IRETD` (`0xcf`).
    Iret = (22, 180, 84, "486 §5 (15 clk) / cmp §3 row 17 (7 clk real)"),
    /// `IRET` in protected mode returning to the SAME privilege level. One flat
    /// 22 served all four modes before slice 8; the census measures that as
    /// 4.4x under at real mode and **14.7x** under here.
    IretPm = (22, 432, 120, "F2 (IRET pm same level 10) / T10.1 (36 clk)"),
    /// `IRET` in protected mode returning to a LOWER privilege level, and the
    /// protected-to-V86 return, which Intel prices together.
    IretPmToV86 = (22, 432, 324, "F2 (IRET pm different level 27) / T10.1 (36 clk)"),
    /// `IRET` executed inside V86 mode.
    IretV86 = (22, 432, 324, "F2 (IRET V86 27) / T10.1 (36 clk)"),

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
    DoubleShift = (3, 48, 60, "F2 (SHLD/SHRD mem by CL 5) / T10.1 (4), slow end"),
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
    Mul8 = (2, 216, 132, "F2 p.F-13 (MUL/IMUL r/m8 11) / T10.1 note 3 (13-18, slow end)"),
    /// `MUL`/`IMUL r/m16`.
    Mul16 = (2, 312, 132, "F2 p.F-13 (11) / T10.1 note 3 (13-26, slow end)"),
    /// `MUL`/`IMUL r/m32`.
    Mul32 = (2, 504, 120, "F2 p.F-13 (10) / T10.1 note 3 (13-42, slow end)"),
    /// `DIV r/m8` -- group 3 `/6` at byte width.
    Div8 = (2, 192, 204, "F2 p.F-13 (DIV r/m8 17) / T10.1 (16)"),
    /// `DIV r/m16`.
    Div16 = (2, 288, 300, "F2 p.F-13 (25) / T10.1 (24)"),
    /// `DIV r/m32` -- the 246x row itself.
    Div32 = (2, 480, 492, "F2 p.F-13 (41) / T10.1 (40) -- design section 9.1's 246x row, sourced digit for digit"),
    /// `IDIV r/m8` -- group 3 `/7` at byte width.
    Idiv8 = (2, 240, 264, "F2 p.F-13 (IDIV r/m8 22) / T10.1 (19 reg, 20 mem, slow end)"),
    /// `IDIV r/m16`.
    Idiv16 = (2, 336, 360, "F2 p.F-13 (30) / T10.1 (27 reg, 28 mem)"),
    /// `IDIV r/m32`.
    Idiv32 = (2, 528, 552, "F2 p.F-13 (46) / T10.1 (43 reg, 44 mem)"),
    /// `INC`/`DEC r/m8` (`0xfe`), today's `INC_DEC_RM8_CORE_CLOCKS`. The memory
    /// form is a read/modify/write, so it takes `AluMemReg`'s shape.
    IncDecRm = (2, 36, 36, "cmp §3 row 4 (3 clk RMW)"),
    /// Two-operand `IMUL r, r/m` (`0x0faf`), both operand forms.
    ImulRm = (9, 504, 120, "F2 p.F-13 (IMUL r,r/m flat 10 at every width) / T10.1 note 3 (13-42 at Dword, slow end); the per-width 486 split wants a width field the JIT kinds do not carry"),
    /// Three-operand `IMUL r, r/m, imm` (`0x69`/`0x6b`).
    ImulImm = (14, 504, 120, "F2 p.F-13 (IMUL r,r/m,imm flat 10) / T10.1 note 3 (13-42 at Dword, slow end); same per-width note"),

    // --- bit operations ------------------------------------------------------
    /// `BT`/`BTS`/`BTR`/`BTC` in both encodings, today's `BIT_STRING_CORE_CLOCKS`.
    BitTest = (6, 96, 108, "F2 (BT 4-9, slow end 9) / T10.1 (BT 3-8, slow end 8)"),
    /// `BTS`/`BTR`/`BTC` in both encodings -- the read/modify/write bit ops,
    /// which lock their memory form and cost 13 clocks on both parts where `BT`
    /// costs 8-9.
    BitTestModify = (6, 156, 156, "F2 (BTS/BTR/BTC mem,reg 13, locked) / T10.1 (13)"),
    /// `BSF`/`BSR` (`0x0fbc`/`0x0fbd`).
    BitScan = (10, 144, 144, "F2 (BSF 6-42, BSR 7-72 MN/MX) / T10.1; data-dependent, typical 12 per the RclRcrCl precedent -- slow end would be 864/1248, recorded not taken"),
    /// `BSWAP` (`0x0fc8..=0x0fcf`).
    Bswap = (1, 12, 12, "F2 (BSWAP 1) / T10.1 (1)"),
    /// `CMPXCHG` (`0x0fb0`/`0x0fb1`).
    CmpXchg = (6, 120, 72, "F2 (CMPXCHG mem 6) / T10.1 (mem 10 locked), slow end"),
    /// `CMPXCHG8B` (`0x0fc7`).
    CmpXchg8b = (10, 120, 120, "F2 (CMPXCHG8B 10); NOT implemented on a 486DX2 -- the I486 entry is unreachable (#UD) and keeps the literal"),
    /// `XADD` (`0x0fc0`/`0x0fc1`).
    Xadd = (4, 48, 48, "F2 (XADD mem 4) / T10.1 (4)"),
    /// `SETcc` (`0x0f90..=0x0f9f`), register and memory forms alike.
    SetCc = (4, 48, 24, "F2 (SETcc mem 2) / T10.1 (4); neither part's rows are condition-keyed"),

    // --- segment and system --------------------------------------------------
    /// `MOV r/m16, Sreg` (`0x8c`), today's `MOV_RM_SREG_CORE_CLOCKS`.
    MovRegSreg = (2, 36, 12, "486 §5 (3 clk) / cmp §3 row 15 (1 clk)"),
    /// `MOV Sreg, r/m` (`0x8e`), today's `MOV_SREG_CORE_CLOCKS`. Real mode; the
    /// protected-mode row (486 108, P5 132 + 96 per unaccessed descriptor)
    /// needs the mode at the charge site and is a later sub-slice.
    MovSregReg = (7, 36, 24, "486 §5 (3 clk real) / cmp §3 row 15 (2 clk real)"),
    /// `LES`/`LDS` (`0xc4`/`0xc5`) and `LFS`/`LGS`/`LSS` (`0x0fb2`..`0x0fb5`).
    LesLds = (7, 72, 96, "F2 (LSS 8 real, slow end of the five) / T10.1 (6 real); protected 12-17 deferred"),
    /// `LAR` (`0x0f02`).
    Lar = (11, 132, 96, "F2 (LAR 8) / T10.1 (11); +8 per unaccessed descriptor (F2 note 9) not modelled"),
    /// `LSL` (`0x0f03`).
    Lsl = (11, 120, 96, "F2 (LSL 8) / T10.1 (10)"),
    /// `VERR`/`VERW` (`0x0f00 /4`, `/5`).
    VerRw = (10, 132, 84, "F2 (VERR/VERW 7) / T10.1 (11)"),
    /// `SLDT`/`STR` (`0x0f00 /0`, `/1`).
    SldtStr = (2, 36, 24, "F2 (SLDT/STR 2) / T10.1 (mem 3), slow end"),
    /// `LLDT`/`LTR` (`0x0f00 /2`, `/3`).
    LldtLtr = (11, 240, 120, "F2 (LTR 10, slow end of the pair) / T10.1 (LTR 20)"),
    /// `SGDT`/`SIDT` (`0x0f01 /0`, `/1`), memory form only.
    SgdtSidt = (11, 120, 48, "F2 (SGDT/SIDT 4) / T10.1 (10)"),
    /// `LGDT`/`LIDT` (`0x0f01 /2`, `/3`).
    LgdtLidt = (11, 144, 72, "F2 (LGDT/LIDT 6) / T10.1 (12)"),
    /// `SMSW` (`0x0f01 /4`).
    Smsw = (2, 36, 48, "F2 (SMSW 4) / T10.1 (mem 3), slow end"),
    /// `LMSW` (`0x0f01 /6`).
    Lmsw = (3, 156, 96, "F2 (LMSW 8) / T10.1 (13)"),
    /// `INVLPG` (`0x0f01 /7`).
    Invlpg = (12, 144, 348, "F2 (INVLPG 29; 25 is often republished -- 29 is the slow side, taken and flagged) / T10.1 (12)"),
    /// `CLTS` (`0x0f06`).
    Clts = (2, 84, 120, "F2 (CLTS 10) / T10.1 (7)"),
    /// `MOV` to and from `CRn`/`DRn` (`0x0f20`..`0x0f23`).
    MovCrDr = (6, 204, 264, "F2 (MOV CR0,r 22, slow end; CR3 21, CR2 12, from CR 4) / T10.1 (CR0 17); a CR0-vs-rest split is worth having, CR3 reloads are the hot ones"),
    /// `BOUND` (`0x62`).
    Bound = (10, 84, 96, "F2 (BOUND in range 8) / T10.1 (7); out of range is INT+32 / INT+24"),
    /// `WRMSR` (`0x0f30`).
    Wrmsr = (30, 360, 540, "F2 (WRMSR 30-45 MN/MX, slow end); no MSRs on a 486DX2 -- the I486 entry is unreachable and keeps the literal"),
    /// `RDTSC` (`0x0f31`).
    Rdtsc = (11, 132, 132, "F2 (RDTSC; PARTIAL -- the extraction cannot separate the clock count from note 11, so the slow reading 11 is kept); no RDTSC on a 486DX2"),
    /// `RDMSR` (`0x0f32`).
    Rdmsr = (11, 132, 288, "F2 (RDMSR 20-24 MN/MX, slow end); no MSRs on a 486DX2 -- I486 keeps the literal"),
    /// `INVD` (`0x0f08`).
    Invd = (4, 48, 180, "F2 (INVD 15) / T10.1 (4)"),
    /// `WBINVD` (`0x0f09`) -- **2000+ clocks on a P5**, against `INVD`'s 15.
    ///
    /// The two shared one class until the manual sourcing found the spread: a
    /// 133x difference on the Pentium and the single largest hole the pass
    /// turned up. The 486's own spread is 4 vs 5 and would never have shown it.
    /// Intel prints "2000+" rather than a count, so this is a floor, not a
    /// measurement -- which is the slow side and the side the 12:15 ruling wants.
    Wbinvd = (4, 60, 24000, "F2 (WBINVD 2000+, a printed floor) / T10.1 (5)"),
    /// `CPUID` (`0x0fa2`).
    Cpuid = (14, 168, 144, "F2 (CPUID 12; 14 is often republished, 12 taken as read and flagged); no CPUID on the DX2 stepping -- I486 keeps the literal"),

    // --- strings and port I/O -----------------------------------------------
    /// One string instruction (`MOVS`/`STOS`/`LODS`/`CMPS`/`SCAS`), today's flat
    /// `STRING_CORE_CLOCKS` for the whole burst.
    ///
    /// Design §3.2 splits this into a per-element cost plus a setup cost, per
    /// family; that split is `RepLimitPlan::compute`'s problem (slice 1, item 5)
    /// and is a later sub-slice, so this stays one flat class.
    StringElem = (4, 48, 48, "PLACEHOLDER x12 -- the per-element split is a later sub-slice"),
    /// `INS`/`INSB`/`INSW`/`INSD` (`0x6c`/`0x6d`).
    InsString = (15, 240, 108, "F3 (INS 9 real) / T10.2 DX2 (20 real; the base i486 datasheet says 17 -- the persona is a DX2-66, so the DX2 column is taken). The bus term is P1's"),
    /// `OUTS`/`OUTSB`/`OUTSW`/`OUTSD` (`0x6e`/`0x6f`).
    OutsString = (14, 240, 156, "F3 (OUTS 13 real) / T10.2 DX2 (20 real; base i486 17). The bus term is P1's"),
    /// `IN AL/eAX, imm8` and `IN AL/eAX, DX`, today's `IN_PORT_CORE_CLOCKS`.
    ///
    /// P1 (`dev_docs/2026-09-05-port-io-repricing-design.md`) owns the epoch-2
    /// port charge and applies it on the BUS side, not here; this row keeps the
    /// core term so a port access does not lose its core cost when the bus term
    /// moves. 486 §5's 168/192 are recorded, not yet reconciled with P1.
    InPort = (12, 204, 168, "T10.2 DX2 (IN 17 real; the base i486 datasheet says 14, and the persona is a DX2-66 so the DX2 column is taken -- recorded). The bus term is P1's"),
    /// `OUT imm8/DX, AL/eAX`, today's `OUT_PORT_CORE_CLOCKS`.
    OutPort = (10, 228, 192, "T10.2 DX2 (OUT 19 real; the base i486 datasheet says 16, and the persona is a DX2-66 so the DX2 column is taken -- recorded). The bus term is P1's"),
    /// `IN eAX, imm8`/`IN eAX, DX` at the dword width, which charge 12 from a
    /// literal rather than through `IN_PORT_CORE_CLOCKS`.
    InPortDword = (12, 204, 168, "T10.2 DX2 (IN 17 real; base i486 14). The bus term is P1's"),

    // --- system events (slice 8) ----------------------------------------------
    // Control-flow-shaped rather than opcode-shaped: these fire on a delivery
    // path, not on a decode. Every one of them was a bare literal in `run.rs`
    // or `control.rs`, and two of them did not exist at all.
    /// Delivering an exception or a V86 monitor trip, from real or protected
    /// mode. `run.rs`'s `Err(Exception)` arm charged a flat 59 for every mode.
    ExceptionDelivery = (59, 828, 480, "F2 INT table, pm trap gate different level 40 / T10.1 (69 clk)"),
    /// Delivering a V86 monitor trip -- the reflected-call path, and census row
    /// 7: **16.7x under**, the worst row in its group.
    ///
    /// `dev_docs/2026-09-05-v86-port-io-timing-research.md` section 1.2
    /// re-anchored it: Intel's V86 / trap gate, different level row is 54 plus
    /// 12 on a cache miss, so 66 clocks, and a reflected V86 `IN` costs that
    /// plus the instruction's own class -- the ~88-100 the research quotes,
    /// once the `IN` itself is added back.
    ExceptionDeliveryV86 = (59, 1032, 792, "F2 INT table, V86/trap gate different level 54 +12 miss / 486 §5 (86 clk)"),
    /// Delivering a maskable hardware interrupt. `run.rs` charged a flat 61.
    /// The INTA cycles the PIC drives are NOT in this number and are not
    /// modelled anywhere -- the census lists 8259A INTA as missing entirely.
    HardwareInterrupt = (61, 372, 192, "F2 INT table, real mode 11 + external INTA / T10.1 (31 clk)"),
    /// A TASK SWITCH: `JMP`/`CALL` through a TSS or task gate, an interrupt
    /// through a task gate, and `IRET` with NT set.
    ///
    /// **This term did not exist.** The switch rode whichever of 17 / 22 / 37 /
    /// 59 delivered it, which the census scores as under by 100x or more. Its
    /// epoch-1 entry is ZERO for exactly that reason -- there was no literal to
    /// preserve, and charging one at epoch 1 would break the knob-unset
    /// identity bar. It is the one class whose epoch-1 value is not a literal
    /// it replaced, and the only class exempt from the non-zero test.
    TaskSwitch = (0, 2388, 2076, "F2 (task switch 173+) / T10.1 (199 clk); NEW, no epoch-1 term existed"),

    // --- x87 -----------------------------------------------------------------
    // Every x87 charge is scaled a second time by `fp_timing_class`, which is a
    // SEPARATE dial and is untouched here (design §4 deletes its `IntConvert32`
    // absorber in slice 4, not now).
    /// `WAIT`/`FWAIT` (`0x9b`) with no pending unmasked exception.
    X87Wait = (6, 72, 12, "F5 (FWAIT 1 latency) / T10.3 via the shipped literal (WAIT 1-3)"),
    /// `FADD`/`FSUB`/`FMUL`/`FDIV`/`FCOM` with an m32 real operand (`0xd8`).
    X87MemArith32 = (20, 240, 36, "F5 (FADD m32 3 latency) / T10.3 via the shipped literal (FADD 20, high end)"),
    /// The same family with an m64 real operand (`0xdc`).
    X87MemArith64 = (20, 240, 36, "F5 (FADD m64 3 latency) / T10.3 via the shipped literal"),
    /// `FIADD`/`FIMUL`/... with an m32 integer operand (`0xda`).
    X87MemArithInt32 = (20, 240, 96, "F5 (FIADD/FISUB/FIMUL 7, FICOM 8 latency -- FIDIV split out) / T10.3 via the shipped literal"),
    /// The same family with an m16 integer operand (`0xde`).
    X87MemArithInt16 = (20, 240, 96, "F5 (integer-operand arith 8 latency without FIDIV) / T10.3 via the shipped literal"),
    /// `FIDIV`/`FIDIVR m32int` (`0xda /6`, `/7`) -- **42 P5 clocks against the
    /// rest of the family's 7-8**, and 85.5 on the 486 against 20.
    ///
    /// Splitting it is worth more than any other x87 split the sourcing found:
    /// without it the whole `0xda` family carries the divide's cost or the
    /// divide carries the adds'.
    X87MemArithIntDiv32 = (20, 1026, 504, "F5 (FIDIV m32 42) / T10.3 (FIDIV m32 Avg 84-86 = 85.5)"),
    /// `FIDIV`/`FIDIVR m16int` (`0xde /6`, `/7`). See `X87MemArithIntDiv32`.
    X87MemArithIntDiv16 = (20, 1044, 504, "F5 (FIDIV m16 42) / T10.3 (FIDIV m16 Avg 85-89 = 87)"),
    /// `FLD m32` (`0xd9 /0`).
    X87LoadReal32 = (14, 168, 12, "cmp §3 row 19 (FLD m32 1 clk)"),
    /// `FST`/`FSTP m32` (`0xd9 /2`, `/3`).
    X87StoreReal32 = (14, 168, 24, "cmp §3 row 19 (FST m32 2 clk)"),
    /// `FLD m64` (`0xdd /0`).
    X87LoadReal64 = (14, 168, 12, "cmp §3 row 19 (FLD m64 1 clk)"),
    /// `FST`/`FSTP m64` (`0xdd /2`, `/3`).
    X87StoreReal64 = (14, 168, 24, "cmp §3 row 19 (FST 2 clk)"),
    /// `FLD m80` (`0xdb /5`).
    X87LoadExtended80 = (14, 168, 36, "F5 (FLD m80 3) / T10.3 via the shipped literal"),
    /// `FSTP m80` (`0xdb /7`).
    X87StoreExtended80 = (14, 168, 36, "F5 (FSTP m80 3) / T10.3 via the shipped literal"),
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
    X87LoadControl = (4, 48, 84, "F5 (FLDCW 7; named unpairable in Optimization Manual 3.6.2.1) / T10.3 (FLDCW 4)"),
    /// `FNSTCW` (`0xd9 /7`).
    X87StoreControl = (14, 168, 24, "F5 (FSTCW 2) / T10.3 via the shipped literal"),
    /// `FNSTSW m16` (`0xdd /7`).
    X87StoreStatus = (14, 168, 60, "F5 (FSTSW m16 5 latency) / T10.3 via the shipped literal"),
    /// `FLDENV` (`0xd9 /4`).
    X87LoadEnv = (44, 528, 444, "F5 (FLDENV 37 real) / T10.3 (FLDENV 44)"),
    /// `FNSTENV` (`0xd9 /6`).
    X87StoreEnv = (56, 672, 600, "F5 (FSTENV 50 real) / T10.3 (FSTENV 56)"),
    /// `FRSTOR` (`0xdd /4`).
    X87Restore = (75, 900, 1140, "F5 (FRSTOR 95 real, 32-bit address form) / T10.3 via the shipped literal (FRSTOR 75)"),
    /// `FNSAVE` (`0xdd /6`).
    X87Save = (150, 1800, 1812, "F5 (FSAVE 151 real, 32-bit address form) / T10.3 (FSAVE 150)"),
    /// `FBLD` (`0xdf /4`).
    X87LoadBcd = (75, 900, 696, "F5 (FBLD 48-58, slow end) / T10.3 (FBLD 75, Intel's own average)"),
    /// `FBSTP` (`0xdf /6`).
    X87StoreBcd = (160, 1920, 1848, "F5 (FBSTP 148-154, slow end) / T10.3 via the shipped literal"),
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
    X87RegCompare = (4, 48, 48, "F5 (FUCOM ST(i) 4 latency) / T10.3 via the shipped literal"),
    /// `FLD ST(i)` and `FXCH` (`0xd9 c0..cf`, `0xd9 d0`).
    X87RegExchange = (4, 48, 12, "design §3.2 (FXCH raw 12)"),
    /// `FCHS` (`0xd9 e0`) and `FABS` (`0xd9 e1`).
    X87RegSign = (6, 72, 12, "F5 (FCHS/FABS 1 latency) / T10.3 (FCHS 6)"),
    /// `FXAM` (`0xd9 e5`) and the four-and-a-half-digit constant loads
    /// `FLDL2T`/`FLDL2E`/`FLDPI`/`FLDLG2`/`FLDLN2` (`0xd9 e9..ed`), which all
    /// charge 8 today. `FXAM` is not a constant load and shares the class only
    /// because it shares the literal; splitting it needs an epoch-2 source it
    /// does not have yet.
    X87RegConst = (8, 96, 60, "F5 (FLDL2T/L2E/PI/LG2/LN2 5 latency) / T10.3 via the shipped literal (8)"),
    /// `FXAM` (`0xd9 e5`) -- **21 P5 clocks**, where the five constant loads it
    /// used to share a literal with are 5. It is not a constant load and never
    /// was; only the shipped `clocks(8)` put them together.
    X87Xam = (8, 96, 252, "F5 (FXAM 21 latency) / T10.3 (FXAM 8)"),
    /// `FTST` (`0xd9 e4`), `FLD1` (`0xd9 e8`) and `FLDZ` (`0xd9 ee`), the three
    /// register-form ops that charge 4 rather than 8 today.
    X87RegConstCheap = (4, 48, 24, "F5 (FLD1/FLDZ 2 latency) / T10.3 via the shipped literal (4)"),
    /// `F2XM1` (`0xd9 f0`).
    X87Exp = (200, 2400, 684, "F5 (F2XM1 13-57, slow end 57) / T10.3 via the shipped literal (200, a round stand-in inside the 486's 140-279 range)"),
    /// The transcendentals that charge 300: `FYL2X`, `FPTAN`, `FPATAN`,
    /// `FSIN`, `FCOS`, `FSINCOS`, `FYL2XP1` (`0xd9 f1..f3`, `f9`, `fb`, `fe`, `ff`).
    X87Transcendental = (300, 3600, 2076, "F5 (FPTAN 17-173, slow end across the family) / T10.3 via the shipped literal (300, a round stand-in inside the 486 ranges)"),
    /// `FXTRACT` (`0xd9 f4`) and `FSQRT` (`0xd9 fa`).
    ///
    /// Design §3.2 puts `FSQRT` at raw 840 on the 586; that number arrives with
    /// the slice-4 ladder that R7 requires, not here.
    X87Sqrt = (70, 840, 840, "design §3.2 (FSQRT 70 clk = raw 840)"),
    /// `FPREM` (`0xd9 f8`) and `FPREM1` (`0xd9 f5`).
    X87Rem = (100, 1200, 840, "F5 (FPREM1 20-70, slow end) / T10.3 via the shipped literal"),
    /// `FRNDINT` (`0xd9 fc`).
    X87RoundInt = (20, 240, 240, "F5 (FRNDINT 9-20, slow end 20) / T10.3 via the shipped literal"),
    /// `FSCALE` (`0xd9 fd`).
    X87Scale = (30, 360, 372, "F5 (FSCALE 20-31, slow end; named unpairable in 3.6.2.1) / T10.3 via the shipped literal"),
    /// `FDECSTP`/`FINCSTP` (`0xd9 f6`/`0xd9 f7`).
    X87StackPointer = (4, 48, 12, "F5 (FINCSTP/FDECSTP 1) / T10.3 via the shipped literal (3)"),
    /// `FNENI`/`FNDISI`/`FNSETPM` (`0xdb e0`/`e1`/`e4`, 387 no-ops) and `FNCLEX`
    /// (`0xdb e2`).
    X87Control = (2, 24, 108, "F5 (FCLEX 9 latency, slow end of the arm) / T10.3 via the shipped literal"),
    /// `FNINIT` (`0xdb e3`).
    X87Init = (3, 36, 192, "F5 (FINIT 16 latency) / T10.3 via the shipped literal (17)"),
    /// `FFREE ST(i)` (`0xdd /0`).
    X87Free = (3, 36, 12, "F5 (FFREE 1) / T10.3 (FFREE 3)"),
    /// `FNSTSW AX` (`0xdf e0`).
    X87StatusReg = (3, 36, 72, "F5 (FSTSW AX 6 latency) / T10.3 via the shipped literal"),
    /// `FST ST(i)` / `FSTP ST(i)` register forms (`0xdd /2`, `/3`).
    X87RegStore = (3, 36, 12, "F5 (FST/FSTP ST(i) 1) / T10.3 via the shipped literal"),
    /// `FCOMPP` (`0xde d9`) and `FUCOMPP` (`0xda e9`) -- compare and pop twice.
    X87ComparePop = (5, 60, 48, "F5 (FCOMPP/FUCOMPP 4 latency) / T10.3 via the shipped literal"),
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

    /// The largest charge in the table -- "the epoch's own maximum-class
    /// constant" the budget bound needs (review B3).
    ///
    /// Taken over every class, including the x87 and system rows a native JIT
    /// slot can never carry, because the bound's job is to DOMINATE and the only
    /// alternative is a hand-maintained mirror of `DirectKind::timing_class`'s
    /// codomain that drifts silently.
    pub(crate) fn max_raw(&self) -> u32 {
        let mut max = 0u16;
        let mut i = 0;
        while i < N_CLASSES {
            if self.0[i] > max {
                max = self.0[i];
            }
            i += 1;
        }
        u32::from(max)
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

/// THE EPOCH-1 TRIPWIRES, checked at compile time.
///
/// `lib.rs`'s per-opcode `*_CORE_CLOCKS` constants no longer feed anything: the
/// charge sites read the class table and, since slice 1c, so does the budget
/// path. They stay because the review asked for them as tripwires, and this
/// block is what makes them one rather than dead code -- each is asserted equal
/// to its class's epoch-1 entry, so a table edit that moves an epoch-1 value
/// fails the BUILD rather than a test.
///
/// `INTERPRET_ONE_MAX_CORE_CLOCKS` and `MAX_CALL_OUT_CORE_CLOCKS` are folds of
/// the others and are asserted against the same folds taken over the table, so
/// the allowlist in `run.rs`'s `INTERPRET_ONE_CLASSES` cannot silently stop
/// covering a row the const covers.
const _: () = {
    const fn entry(class: TimingClass) -> u32 {
        EPOCH1_ENTRIES[class.index()] as u32
    }
    assert!(entry(TimingClass::PopMem) == crate::POP_RM_CORE_CLOCKS);
    assert!(entry(TimingClass::MovRegSreg) == crate::MOV_RM_SREG_CORE_CLOCKS);
    assert!(entry(TimingClass::Xchg) == crate::XCHG_CORE_CLOCKS);
    assert!(entry(TimingClass::BitTest) == crate::BIT_STRING_CORE_CLOCKS);
    assert!(entry(TimingClass::BitTestModify) == crate::BIT_STRING_CORE_CLOCKS);
    // Group 3's thirteen classes all replaced ONE `clocks(GROUP3_CORE_CLOCKS)`,
    // so all thirteen carry its literal at epoch 1. That is the whole reason the
    // split is invisible under the knob-unset identity fixture.
    assert!(entry(TimingClass::TestImmReg) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::TestImmMem) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::NotNegReg) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::NotNegMem) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Mul8) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Mul16) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Mul32) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Div8) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Div16) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Div32) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Idiv8) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Idiv16) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::Idiv32) == crate::GROUP3_CORE_CLOCKS);
    assert!(entry(TimingClass::IncDecRm) == crate::INC_DEC_RM8_CORE_CLOCKS);
    assert!(entry(TimingClass::PushMem) == crate::PUSH_RM_CORE_CLOCKS);
    assert!(entry(TimingClass::Cli) == crate::CLI_CORE_CLOCKS);
    assert!(entry(TimingClass::Sti) == crate::STI_CORE_CLOCKS);
    assert!(entry(TimingClass::MovSregReg) == crate::MOV_SREG_CORE_CLOCKS);
    assert!(entry(TimingClass::PopSs) == crate::POP_SS_CORE_CLOCKS);
    assert!(entry(TimingClass::StringElem) == crate::STRING_CORE_CLOCKS);
    assert!(entry(TimingClass::PushFlags) == crate::PUSHF_CORE_CLOCKS);
    assert!(entry(TimingClass::PopFlags) == crate::POPF_CORE_CLOCKS);
    assert!(entry(TimingClass::InPort) == crate::IN_PORT_CORE_CLOCKS);
    assert!(entry(TimingClass::InPortDword) == crate::IN_PORT_CORE_CLOCKS);
    assert!(entry(TimingClass::OutPort) == crate::OUT_PORT_CORE_CLOCKS);
    assert!(entry(TimingClass::PushAll) == crate::PUSH_ALL_CORE_CLOCKS);
    assert!(entry(TimingClass::PopAll) == crate::POP_ALL_CORE_CLOCKS);
    assert!(entry(TimingClass::IntN) == crate::INT_IMM8_CORE_CLOCKS);
    assert!(entry(TimingClass::Lar) == crate::LAR_LSL_CORE_CLOCKS);
    assert!(entry(TimingClass::Lsl) == crate::LAR_LSL_CORE_CLOCKS);

    // The two folds. `INTERPRET_ONE_MAX_CORE_CLOCKS` is the maximum over the
    // allowlist; the budget path takes the same maximum over
    // `run.rs`'s `INTERPRET_ONE_CLASSES`, and this asserts the two agree at
    // epoch 1 -- which is the only epoch at which they can, since the const
    // cannot be persona-keyed.
    let mut interpret_one = 0u32;
    let mut i = 0;
    while i < crate::run::INTERPRET_ONE_CLASSES.len() {
        let value = entry(crate::run::INTERPRET_ONE_CLASSES[i]);
        if value > interpret_one {
            interpret_one = value;
        }
        i += 1;
    }
    assert!(interpret_one == crate::INTERPRET_ONE_MAX_CORE_CLOCKS);

    let mut max = interpret_one;
    if entry(TimingClass::InPort) > max {
        max = entry(TimingClass::InPort);
    }
    if entry(TimingClass::OutPort) > max {
        max = entry(TimingClass::OutPort);
    }
    if entry(TimingClass::PushAll) > max {
        max = entry(TimingClass::PushAll);
    }
    if entry(TimingClass::PopAll) > max {
        max = entry(TimingClass::PopAll);
    }
    if entry(TimingClass::IntN) > max {
        max = entry(TimingClass::IntN);
    }
    if entry(TimingClass::Lar) > max {
        max = entry(TimingClass::Lar);
    }
    assert!(max == crate::MAX_CALL_OUT_CORE_CLOCKS);
};

/// Per-class retire counts, plus the one honesty counter that keeps them
/// readable (design section 9.1, and review R5, which makes this the load-bearing
/// falsifier now that Dhrystone is demoted).
///
/// # What it counts, and what it cannot
///
/// The interpreter increments exactly once per retire, in `CpuGsw::charge`. The
/// JIT cannot: a compiled block's instructions retire inside emitted code that
/// never calls `charge`, so a native ENTRY adds its block's compile-time class
/// vector instead -- design section 9.1's "sparse list", walked once per entry
/// rather than once per instruction.
///
/// That attribution is exact for an entry that runs one block, and for a
/// self-loop, whose retires are that same vector repeated. It is NOT exact for a
/// CHAINED entry: `NativeExit::instructions` counts the whole chain from its
/// head, and only the head block's vector is in hand. Those instructions are
/// counted in [`TimingHistogram::unattributed`] rather than spread over the head
/// block's classes, so a share read off this table is a share of the
/// ATTRIBUTED population and the reader can see how much is missing. Anything
/// else would invent a distribution.
///
/// Slots whose charge arrives at run time -- x87 (through `weighted_fp_clocks`)
/// and call-outs (through the helper's return value) -- carry no class and are
/// absent from a block's vector, so they are unattributed too.
/// The marker a block's class vector carries for a slot with no class -- an x87
/// or call-out slot, whose charge arrives at run time. `N_CLASSES` is 157, so
/// `u8::MAX` can never collide with a real index; a static assertion below says
/// so rather than leaving it to arithmetic.
pub(crate) const UNCLASSED_SLOT: u8 = u8::MAX;

const _: () = assert!(
    N_CLASSES < UNCLASSED_SLOT as usize,
    "a class index would collide with UNCLASSED_SLOT; the vector needs a wider element"
);

#[derive(Clone)]
pub(crate) struct TimingHistogram {
    counts: [u64; N_CLASSES],
    /// Slice 8's system events, counted apart from retires. See
    /// `record_system_event`.
    system_events: Box<[u64; N_CLASSES]>,
    unattributed: u64,
}

/// ALWAYS EQUAL, on the `FarCallLedger` precedent (`lib.rs`), and for its exact
/// reason: `CpuGsw` derives `PartialEq` and the differential tests compare whole
/// CPUs, so a diagnostic counter that compared by value would make the native
/// and interpreted legs of every sweep unequal on a field no guest instruction
/// can observe. The two legs' histograms differ legitimately -- the interpreter
/// attributes per retire and the JIT per block entry, and a chained entry lands
/// in `unattributed` on one side only.
impl PartialEq for TimingHistogram {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for TimingHistogram {}

/// 157 rows of mostly zeros stay out of every `{:?}` dump.
impl std::fmt::Debug for TimingHistogram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TimingHistogram {{ {} attributed, {} unattributed }}",
            self.attributed(),
            self.unattributed
        )
    }
}

impl Default for TimingHistogram {
    fn default() -> Self {
        Self {
            counts: [0; N_CLASSES],
            system_events: Box::new([0; N_CLASSES]),
            unattributed: 0,
        }
    }
}

impl TimingHistogram {
    /// One interpreter retire.
    #[inline]
    pub(crate) fn record(&mut self, class: TimingClass) {
        match class {
            // A `Legacy` site has no class row, so it cannot be attributed. It
            // is one site (ARPL) and it is counted honestly rather than folded
            // into a neighbour.
            TimingClass::Legacy(_) => self.unattributed += 1,
            other => self.counts[other.index()] += 1,
        }
    }

    /// One compiled-block entry: `passes` complete traversals of `vector` plus a
    /// `remainder`-long prefix of it, which is exactly how a block's slots
    /// retire on a completed run, a self-loop and a mid-block side exit alike.
    ///
    /// `vector` carries ONE ENTRY PER SLOT, with [`UNCLASSED_SLOT`] for the x87
    /// and call-out slots whose charge arrives at run time. Those retires go to
    /// `unattributed`: they did happen, and they have no class to hold them.
    pub(crate) fn record_block(&mut self, vector: &[u8], passes: u64, remainder: usize) {
        for (position, index) in vector.iter().enumerate() {
            let times = passes + u64::from(position < remainder);
            if times == 0 {
                continue;
            }
            if *index == UNCLASSED_SLOT {
                self.unattributed += times;
            } else {
                self.counts[usize::from(*index)] += times;
            }
        }
    }

    /// Instructions this histogram knows retired but cannot place in a class.
    pub(crate) fn record_unattributed(&mut self, instructions: u64) {
        self.unattributed += instructions;
    }

    /// One SYSTEM EVENT: a delivery, a task switch, a mode-keyed far transfer.
    ///
    /// Kept apart from the retire counts on purpose. A system event is not a
    /// retire -- an exception delivery charges clocks without retiring an
    /// instruction, and a task switch charges them from inside `control.rs` --
    /// so folding either into `counts` would make `class_clocks / attributed`
    /// stop meaning clocks per retired instruction. Slice 8 is the slice that
    /// makes these numbers move, and this is how their contribution is read.
    pub(crate) fn record_system_event(&mut self, class: TimingClass) {
        if let TimingClass::Legacy(_) = class {
            return;
        }
        self.system_events[class.index()] += 1;
    }

    /// `(class name, count)` for every system event that fired.
    pub(crate) fn system_event_rows(&self) -> Vec<(&'static str, u64)> {
        TimingClass::ALL
            .iter()
            .filter(|class| self.system_events[class.index()] != 0)
            .map(|class| (class.name(), self.system_events[class.index()]))
            .collect()
    }

    /// The clocks those system events cost under `table`.
    pub(crate) fn system_event_clocks(&self, table: &ClassTable) -> u64 {
        TimingClass::ALL
            .iter()
            .map(|class| u64::from(table.raw(*class)) * self.system_events[class.index()])
            .sum()
    }

    /// `(class name, count)` for every class with a nonzero count, plus the
    /// unattributed total. Sparse on purpose: 157 rows of mostly zeros in a
    /// profile JSON is noise, and the reader needs the shares.
    pub(crate) fn rows(&self) -> Vec<(&'static str, u64)> {
        TimingClass::ALL
            .iter()
            .filter(|class| self.counts[class.index()] != 0)
            .map(|class| (class.name(), self.counts[class.index()]))
            .collect()
    }

    /// The class clocks these retires cost under `table` -- the numerator of
    /// review R5(i)'s serial, pre-pairing class term.
    pub(crate) fn class_clocks(&self, table: &ClassTable) -> u64 {
        TimingClass::ALL
            .iter()
            .map(|class| u64::from(table.raw(*class)) * self.counts[class.index()])
            .sum()
    }

    pub(crate) fn attributed(&self) -> u64 {
        self.counts.iter().sum()
    }

    pub(crate) fn unattributed(&self) -> u64 {
        self.unattributed
    }
}
