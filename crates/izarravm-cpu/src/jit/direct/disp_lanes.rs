// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The three DISPLACEMENT lane matchers: which slots may read their disp32 out of guest RAM on
//! every execution instead of baking it into the emitted address arithmetic.
//!
//! One file because the three share every bar but their opcode and their kind, and because the
//! two Option D arms are read against the `0x8A` family they extend:
//!
//! | matcher | shapes | knob | default | counters |
//! |---|---|---|---|---|
//! | `disp_lane_for` | `0x8A` mem, disp32 | `IZARRAVM_DISP_LANES` | ON | `smc_disp_lane_*` |
//! | `disp_load_widen_lane_for` | `0x8B` mem, disp32 | `IZARRAVM_DISP_LOAD_WIDEN` | OFF | `smc_disp_load_widen_lane_*` |
//! | `disp_store_lane_for` | `0x89` / `0x88` mem, disp32 | `IZARRAVM_DISP_STORE_LANES` | **ON** | `smc_disp_store_lane_*` |
//!
//! Two of the three ship ON. The store arm flipped on the 2026-08-23 Option D ladder
//! (`duke3d-586` long 259.6 -> 194.8 s, −25.0%, corpus inert); the `0x8B` widening stays off
//! because at `MAX_BLOCK_IMM_LANES` = 12 it competes for the budget instead of adding capture,
//! and it is blocked on the cap re-price rather than refuted. Both knob docs carry the tables.
//!
//! `disp_lane_for` moved here from `direct.rs` to keep that file under the layout limit. ONE
//! TOKEN changed in the move and it is named here so "unchanged" is checkable rather than
//! asserted: `fn disp_lane_for` became `pub(crate) fn disp_lane_for`, because the compile walk
//! that calls it is now in the parent module. Its body, its bars, its ordering and every line of
//! its doc comment are byte-identical to `f6620e6e`.
//!
//! The two new arms are PRIVATE. Only `disp_lane_for` (the walk calls it directly, ahead of these
//! two) and `option_d_lane_for` (the walk's one entry to both new arms) leave this module, so the
//! two knob tests and the arm selection cannot be reached from anywhere that could skip one.
//!
//! # The shape of the two new arms, and what is deliberately NOT in them
//!
//! Both are `disp_lane_for` with one bar moved. The kind test, the prefix test, `disp_len == 4`,
//! `imm_len == 0`, the `physical + len - 4` lane start, the `has_record_range` heat gate, the
//! `direct_host_bytes` page guard and the cap-under-the-heat-gate ordering are all identical,
//! and that is the point: the three families then share one denominator and one failure mode,
//! and a ladder that moves one of them can be read against the other two.
//!
//! * **disp32 only.** A disp8 or disp16 lane would need sign-extension at load time and a
//!   sub-4-byte rule in the write choke, which matches a patch at exactly the lane's width. The
//!   2026-08-23 settling census counted the `0x89` disp population at 1,034,337 events with the
//!   whole prize in the disp32 forms; no measured population asks for a narrower lane. This is
//!   the same scope cut `dev_docs/2026-08-09-disp-lanes-design.md` made for `0x8A`.
//! * **No prefixes.** That bar carries the width argument for all three: a `0x66` or `0x67`
//!   instruction is refused outright, and a disp32 requires CS.D = 1, so an admitted `0x8B` or
//!   `0x89` is a DWORD access, an admitted `0x88` is a byte access, and `AddressWrap::Word` can
//!   never co-occur with a lane in any of them.
//! * **Memory forms only**, which is not a test either arm has to write: `classify` lowers the
//!   `mod == 3` encodings of all four opcodes to register kinds (`MovReg` / `MovRegByte`), so a
//!   register form never reaches a `Load` or a `Store` arm at all.
//!
//! # Why a `Store` may carry a lane — the register-pressure contract, discharged
//!
//! `disp_lane_for`'s own doc states the obligation: the lane arm of `emit_effective_address`
//! stages the displacement through RAX alone, "but widening admission to a kind whose emitter
//! resolves the address AFTER staging other live state would still deserve its own review".
//!
//! `Store` is not such a kind, checked at the line in both emitters:
//!
//! * `emit_store` calls `emit_segmented_linear_address` as its first emission; RCX, RDX, RDI and
//!   the store value are all produced after it.
//! * `emit_store_fast` (the `IZARRAVM_ONE_LOOKUP_STORE` path) does the same: the address is
//!   formed first, then `emit_store_bias_probe` (RCX/RDI) and `emit_read_store_value` (RDX).
//!
//! So nothing is live across the address form at either site, and the lane arm's RAX write is
//! indistinguishable from the baked arm's `mov eax, imm32` to every caller. The `scale != 1`
//! index path clobbers RCX in BOTH arms; that is `emit_effective_address`'s pre-existing
//! contract, not the lane's, and it is as true for a store as it is for a load.
//!
//! # Semantics
//!
//! A patched displacement forms a different effective address, and every guard downstream of
//! `emit_effective_address` — the 64K wrap, the segment-limit compare, the fast-map lookup, the
//! page-kind classify, the write-permission check and the code-watch consultation — already runs
//! on the RUNTIME value. A store through a patched displacement therefore takes exactly the side
//! exits the baked form would have taken, including a store that now lands on a watched code
//! page: the watch consultation is inside `emit_store_write_resolve`, after the address exists.
//! That is what makes the widening a lane admission rather than a new coherence question.
//!
//! A block that patches its OWN displacement field exits through the code-watch side before the
//! store lands, which is the argument the imm lanes and `0x8A` already rely on, unchanged.

use super::*;

/// The displacement twin of `imm_lane_for`: `0x8A MOV r8, [..disp32..]`, every ModRM memory
/// form, no prefixes — GATED ON MEASURED PATCH HISTORY. The admitted field is the
/// instruction's disp32, which duke3d-586's SMC trace measured at 17M of its 19.3M disp-patch
/// events (dev_docs/2026-08-09-disp-lanes-design.md); each one today either kills the covering
/// block or keeps its chunk's G1 heat stamped.
///
/// THE HEAT GATE IS THE SLICE'S LOAD-BEARING DECISION, and it was reached by refutation twice
/// over. The lane form costs two host instructions per EXECUTION whether or not the field is
/// ever patched. Iteration 1 admitted the whole family unconditionally: duke +8.2%, but the
/// 2026-08-09 formal gate FAILED — doom-486 paired RTF 0.978, doom-586 0.975 — because doom's
/// renderer executes `[base+disp32]` texture/colormap byte loads constantly and patches none
/// of them. Iteration 2 tried the shape cut (bare `[disp32]` only): doom recovered but duke's
/// win VANISHED (rt 0.2706 vs 0.2697, 3.4k lanes vs 233k) — Build patches the indexed forms
/// too, so no static shape separates the populations. What separates them is BEHAVIOR:
/// `SmcHeatMap::has_record_range` over the disp field's bytes is true only after the field
/// took a heat-charged kill, so a never-patched load compiles baked and untaxed forever, and a
/// patched one converges to the lane form one kill after its first patch (the kill bumps the
/// record, the recompile sees it). Lane-absorbed patches deliberately do not refresh records,
/// and a record consumed by `lift_cold_smc_dormant` recovery self-heals the same way: one more
/// kill, one more recompile.
///
/// The probe reads the heat accelerator WITHOUT `sync_smc_heat` (this is a `&CpuGsw` path); a
/// stale read across a cache reset can at worst bake one block that a later recompile lanes, or
/// lane one block that did not need it — admission tuning, never correctness.
///
/// `disp_len == 4` plus the default-prefix test confines this to 32-bit addressing: a CS.D=0
/// segment cannot reach a four-byte displacement without a `0x67` prefix, so a lane and
/// `AddressWrap::Word` can never co-occur and the loaded field needs no sign-extension — the
/// four guest bytes ARE the architectural displacement. With `imm_len == 0` those bytes are
/// the instruction's last four, so the lane start is `physical + len - 4` (offset 2 on the
/// mod-0 rm-5 form, 3 on the SIB forms, more under mod 2 — the SIB fixture pins this).
///
/// Only `DirectKind::Load` may carry a lane, and that is a REGISTER-PRESSURE contract, not
/// taste: the lane arm of `emit_effective_address` stages the displacement through EAX alone,
/// which is safe for every caller, but widening admission to a kind whose emitter resolves the
/// address AFTER staging other live state would still deserve its own review — and its own
/// census row, per the standing rule against unmeasured admissions.
pub(crate) fn disp_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
    cap_refusals: &mut LaneCapRefusals,
) -> Option<(DirectKind, ImmLane)> {
    if !disp_lanes_enabled() {
        return None;
    }
    let DirectKind::Load {
        dst,
        width,
        addr,
        raw_clocks,
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x8a
        || insn.prefixes != Prefixes::default()
        || insn.disp_len != 4
        || insn.imm_len != 0
    {
        return None;
    }
    let lane = physical.checked_add(u32::from(insn.len).checked_sub(4)?)?;
    if !cpu
        .jit_direct
        .smc_heat
        .has_record_range(lane, IMM_LANE_WIDTH)
    {
        return None;
    }
    // Page-local in physical for the same reason as `imm_lane_for`: the compile loop only
    // reaches this after `physical_page_local`, so one host pointer covers all four bytes.
    let host = cpu.direct_host_bytes(lane, IMM_LANE_WIDTH)?;
    // The cap, split off the knob above and tested LAST, which is where this family differs from
    // the other three. The bar it must stay under is the heat gate: that is what separates a
    // patched load from doom's never-patched texture reads, so a cap test placed above it would
    // report the whole `0x8A` population as budget pressure. It stays under `direct_host_bytes`
    // as well, unlike the other three, because the gate above it is the expensive bar here (a
    // `has_record_range` hash lookup) and hoisting over the handful-of-entries fetch-cache scan
    // under it would buy nothing measurable. That leaves one family in which the page guard is
    // pinned ABOVE the cap, which is what
    // `a_disp_slot_whose_lane_bytes_are_not_direct_mapped_charges_no_cap_refusal` holds.
    if lanes_used >= MAX_BLOCK_IMM_LANES {
        cap_refusals[LANE_CAP_DISP] = cap_refusals[LANE_CAP_DISP].saturating_add(1);
        return None;
    }
    let lane = ImmLane {
        physical: lane,
        host,
        width: IMM_LANE_WIDTH as u8,
    };
    Some((
        DirectKind::Load {
            dst,
            width,
            addr: DirectAddr {
                disp_lane: Some(lane),
                ..addr
            },
            raw_clocks,
        },
        lane,
    ))
}

/// Which Option D arm claimed a slot, so the compile walk can charge the right registration
/// counter without re-deriving the opcode.
///
/// An enum rather than a `bool` because the two arms are separately knobbed, separately counted
/// and separately censused, and a bare `true` at the call site would read as "is a lane".
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OptionDArm {
    /// `0x89` / `0x88`, the store arm behind `IZARRAVM_DISP_STORE_LANES`.
    Store,
    /// `0x8B`, the load widening behind `IZARRAVM_DISP_LOAD_WIDEN`.
    LoadWiden,
}

/// The bars the two Option D arms share, in `disp_lane_for`'s exact order and with its exact
/// meanings. Returns the lane, or `None` for any refusal; the cap refusal is charged to `family`
/// on the way out and nowhere else.
///
/// ORDER IS THE CONTRACT, not a detail:
///
/// 1. the shape bars (prefixes, `disp_len`, `imm_len`) — cheapest, and they are what narrow the
///    slot to one family, which is what makes the cap counter below per-family;
/// 2. the lane start `physical + len - 4`, which is the instruction's last four bytes exactly
///    because `imm_len == 0` was just established;
/// 3. **the heat gate** `has_record_range` — a record exists only after a heat-charged kill at
///    those bytes, so a never-patched displacement compiles baked and untaxed forever. This is
///    the doom cut, and it is why the cap sits BELOW it: a cap tested above the heat gate would
///    report the entire `0x89` population as budget pressure;
/// 4. `direct_host_bytes`, the page guard;
/// 5. **the cap**, last, exactly as in `disp_lane_for`. Its counter is therefore the tighter of
///    the two definitions the four shipped families use: every slot it counts had a heat record
///    AND a host pointer waiting.
///
/// The knob is NOT here. Each arm tests its own knob first and returns before reaching this
/// function, so an off arm charges no cap refusal — the rule that lets a ladder tell a
/// lane-BUDGET answer from a lane-CLASS one.
fn shared_disp32_lane(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    physical: u32,
    lanes_used: usize,
    cap_refusals: &mut LaneCapRefusals,
    family: usize,
) -> Option<ImmLane> {
    // `imm_len == 0` IS UNREACHABLE FOR TODAY'S TWO CALLERS and is kept anyway, recorded rather
    // than quietly left in: the mutation run for this slice deleted it and every fixture stayed
    // green, because no `0x88`, `0x89` or `0x8B` encoding carries an immediate at all, so the
    // opcode bars above already imply it. It stays because this helper is the seam a third arm
    // will call, `physical + len - 4` is only "the displacement" when nothing follows it, and a
    // widening that reached `0xC7 MOV r/m32, imm32` without it would lane the stored VALUE. The
    // `the_store_arm_refuses_every_shape_outside_its_bars` list carries that shape for the day
    // the opcode bar moves.
    if insn.prefixes != Prefixes::default() || insn.disp_len != 4 || insn.imm_len != 0 {
        return None;
    }
    let lane = physical.checked_add(u32::from(insn.len).checked_sub(4)?)?;
    if !cpu
        .jit_direct
        .smc_heat
        .has_record_range(lane, IMM_LANE_WIDTH)
    {
        return None;
    }
    // Page-local in physical for `imm_lane_for`'s reason: the compile loop only reaches a lane
    // matcher after `physical_page_local`, so one host pointer covers all four bytes.
    let host = cpu.direct_host_bytes(lane, IMM_LANE_WIDTH)?;
    if lanes_used >= MAX_BLOCK_IMM_LANES {
        cap_refusals[family] = cap_refusals[family].saturating_add(1);
        return None;
    }
    Some(ImmLane {
        physical: lane,
        host,
        width: IMM_LANE_WIDTH as u8,
    })
}

/// The STORE arm of Option D: `0x89 MOV r/m32, r32` and `0x88 MOV r/m8, r8`, every ModRM MEMORY
/// form with a disp32 and no prefixes, gated on measured patch history exactly as `0x8A` is.
///
/// `0x89` is the prize. The 2026-08-23 settling census puts it at 1,034,337 disp-class SMC events
/// and 21,178 of the 21,882 joined un-laned-disp BLOCK KILLS on duke3d-586-short — 96.8% —
/// against `0x88`'s 37,717 events and a numerator that never joins uniquely. `0x88` is admitted
/// with it because it is the same instruction one width down, its emitter is the same
/// `emit_store`, and refusing it would be an unexplained hole rather than a scope cut.
///
/// The lane rides on the ADDRESS (`DirectAddr::disp_lane`), so the store's SOURCE is untouched:
/// `StoreSource::Reg` still reads its home slot, and a `StoreSource::Selector` still bakes the
/// entry selector. Only where the displacement comes from changes.
///
/// Refuses everything that is not a `Store` kind, which includes `RmwIncDec`, `SetCcMem`,
/// `AluMemDest` and the x87 stores. Those are unmeasured admissions with emitters this arm's
/// review did not cover, and they wait for a census row of their own — the standing rule the
/// register-pressure contract states.
fn disp_store_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
    cap_refusals: &mut LaneCapRefusals,
) -> Option<(DirectKind, ImmLane)> {
    if !disp_store_lanes_enabled() {
        return None;
    }
    let DirectKind::Store {
        source,
        width,
        addr,
        raw_clocks,
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x89 && insn.opcode != 0x88 {
        return None;
    }
    let lane = shared_disp32_lane(
        cpu,
        insn,
        physical,
        lanes_used,
        cap_refusals,
        LANE_CAP_DISP_STORE,
    )?;
    Some((
        DirectKind::Store {
            source,
            width,
            addr: DirectAddr {
                disp_lane: Some(lane),
                ..addr
            },
            raw_clocks,
        },
        lane,
    ))
}

/// The LOAD-WIDENING arm of Option D: `0x8B MOV r32, r/m32`, every ModRM MEMORY form with a
/// disp32 and no prefixes.
///
/// The cheap half — same `DirectKind::Load`, same emitter, same `DirectAddr` seam as `0x8A`, and
/// the only thing that changes is which opcode reaches it. 177,730 disp-class events on the
/// settling census against `0x89`'s 1,034,337, and 1,346 of the 12,887 joined un-laned-disp
/// crossings. It is not worth shipping alone and it is not optional either: the pre-registered
/// capture line is 11.9%, the store arm alone reads 11.460% and the pair reads 12.797%.
///
/// A SEPARATE FUNCTION rather than an opcode-set widening inside `disp_lane_for`, and that is a
/// measurement decision. Folding `0x8b` into `disp_lane_for`'s opcode test would put both
/// populations behind one knob and one counter pair, and the ladder could then not attribute a
/// movement to the arm that caused it. It also keeps the shipped `0x8A` matcher byte-identical
/// under this change, which is what makes the OFF-arm identity claim checkable rather than
/// argued.
fn disp_load_widen_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
    cap_refusals: &mut LaneCapRefusals,
) -> Option<(DirectKind, ImmLane)> {
    if !disp_load_widen_enabled() {
        return None;
    }
    let DirectKind::Load {
        dst,
        width,
        addr,
        raw_clocks,
    } = kind
    else {
        return None;
    };
    if insn.opcode != 0x8b {
        return None;
    }
    let lane = shared_disp32_lane(
        cpu,
        insn,
        physical,
        lanes_used,
        cap_refusals,
        LANE_CAP_DISP_LOAD_WIDEN,
    )?;
    Some((
        DirectKind::Load {
            dst,
            width,
            addr: DirectAddr {
                disp_lane: Some(lane),
                ..addr
            },
            raw_clocks,
        },
        lane,
    ))
}

/// The two Option D arms as one call, for the compile walk.
///
/// The walk already nests four matchers; a fifth and sixth level of `match` would be unreadable
/// and would say nothing, because the two arms are mutually exclusive by KIND (`Store` against
/// `Load`) and by OPCODE. Trying the store first is therefore an ordering with no observable
/// consequence — it is written prize-first so the common case is the first test.
pub(crate) fn option_d_lane_for(
    cpu: &CpuGsw,
    insn: &DecodedInsn,
    kind: DirectKind,
    physical: u32,
    lanes_used: usize,
    cap_refusals: &mut LaneCapRefusals,
) -> Option<(DirectKind, ImmLane, OptionDArm)> {
    if let Some((kind, lane)) =
        disp_store_lane_for(cpu, insn, kind, physical, lanes_used, cap_refusals)
    {
        return Some((kind, lane, OptionDArm::Store));
    }
    let (kind, lane) =
        disp_load_widen_lane_for(cpu, insn, kind, physical, lanes_used, cap_refusals)?;
    Some((kind, lane, OptionDArm::LoadWiden))
}
