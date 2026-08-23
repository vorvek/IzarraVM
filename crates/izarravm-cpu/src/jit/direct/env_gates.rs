// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Process-global environment gates for the Direct backend, and the two persona predicates that
//! read them.
//!
//! Every knob here is a `OnceLock` resolved once per process from a `IZARRAVM_*` variable, so a
//! gate is a load and a branch on the compile path rather than a `getenv`. They live together
//! because they are read together: `classify` asks four of them per instruction, and keeping the
//! whole family in one file is what makes "which rows does this build admit" answerable by
//! reading one place.
//!
//! Moved out of `direct.rs` unchanged, to keep that file under the layout limit. No gate, no
//! default and no doc comment changed in the move.

use crate::{CpuGsw, CpuPersona};

/// Admission level for 16-bit code segments. **DEFAULT 1 since the 486 measurement**; it used to
/// be 0, and the doc comment used to say "this exists to price a lever, not to ship".
///
///   0  refuse every 16-bit code segment (the old default, still the off switch)
///   1  admit 16-bit (CS.D = 0) code segments backed by ordinary RAM
///   2  additionally admit the 0xC0000..0x100000 option-ROM + BIOS window
///
/// Level 2 deliberately stops at 0xC0000 and leaves 0xA0000..0xC0000 (VGA memory) refused: that
/// half of the window is a device aperture with read side effects, and it is the half the original
/// guard was really about.
///
/// **Level 2 is measured WASTE and should not be used.** On a PoP boot it produces 531 extra
/// compile attempts and ZERO extra installs, because `install`'s page-cover check wants a RAM
/// direct page and ROM is not one. The admission gate and the installer disagree about the same
/// window. Fix or retire it; do not set it hoping for BIOS coverage.
///
/// What flipping the default to 1 buys and costs, measured on a quiet box, min-of-N:
///
///   * PoP-486, a real-mode game: coverage 1.03% -> 74.47%, 9.68 native insns/entry, wall NEUTRAL,
///     framebuffer bit-identical over 4e9 cycles.
///   * quake-586: **+4.14% slower**. Its 16-bit code is 55% of entries at 2.431 insns/entry, i.e.
///     DOS/BIOS/extender glue in blocks too short to amortise a dispatcher entry.
///
/// That split is a WORKLOAD SHAPE, not a persona: real-mode game loops win, a 32-bit game's 16-bit
/// glue loses.
///
/// Defaulted ON at parity deliberately, and the reasoning is pre-release reasoning: there is no
/// version out, so a default is a development posture rather than a promise to anyone. On costs a
/// measured 4% on one workload and buys exposure of the 16-bit path to every fixture, every gate
/// run and every future slice, which is how the remaining coverage work gets found and how each
/// lowering lands as upside instead of paying down a deficit. Revisit the trade before a release,
/// not before then. Closing the quake gap is the next objective.
pub(crate) fn sixteen_bit_admission_level() -> u8 {
    static LEVEL: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("IZARRAVM_JIT16")
            .ok()
            .and_then(|value| value.parse::<u8>().ok())
            .unwrap_or(1)
    })
}

/// Whether a hot SMC chunk may spend one compile-through-heat "lane trial" per key per heat
/// epoch (`IZARRAVM_SMC_LANE_TRIAL`, ON for every value except exactly "0"). Per KEY, not per
/// chunk, deliberately: N entry points inside one 16-byte chunk buy N trials per epoch, which
/// stays bounded and lets each entry's own lane coverage decide its fate.
///
/// DEFAULT ON SINCE THE DISP LANES LANDED, and the flip is the same measurement that once
/// turned it off, repeated on the other side of its stated precondition. 2026-08-08
/// (duke586-lanetrial-{0,1}.json), imm lanes only: 146,956 trials, 61,216 installs, rt -5.5%
/// (0.2600 -> 0.2456) — Build's patch bursts mix lane-shaped 0x81 writes with disp-field 0x8A
/// rewrites in the same chunks, so trial installs died to the writes the lanes could not
/// absorb, and the doc said "re-measure after displacement lanes exist". 2026-08-09
/// (duke586-displane3-{0,1,trial}.json), heat-gated disp lanes shipped: trial-off is INERT on
/// duke3d-586 (rt 0.2443 vs 0.2445 off-arm, because the kill that writes a disp lane's heat
/// record also heats the chunk toward this very gate, so the laned recompile mostly never
/// installs), and trial-on is rt 0.2801, +14.6%, with 446,503 disp lanes registered and
/// narrow kills down 0.9M. The lanes and the trial are ONE mechanism: the trial is how a laned
/// block gets past the heat gate, the lanes are why its install survives. doom-486/586 take
/// ZERO disp lanes (heat-gated admission) and measured neutral in the same sitting.
///
/// WHY THE TRIAL EXISTS. G1's admission gates and the mutable-lane mechanism deadlock against
/// each other on a fixture whose patch loop never pauses: the gate refuses to compile while the
/// chunk is hot, so no block exists, so no lanes register, so every patch narrow-kills decode
/// lines and re-stamps the heat, forever. Duke3d spends 44.8% of its dispatcher seams exiting
/// into exactly this state (dev_docs/2026-08-08-dispatch-tier-next.md). Doom never hit the
/// deadlock only because its blocks compiled BEFORE the heat crossed the threshold.
///
/// The trial breaks the cycle with a bounded probe: one compilation per key per heat epoch is
/// allowed THROUGH the hot gate; it installs only if it registered at least one mutable lane.
/// From there the mechanism self-selects. If the lanes cover the guest's patches, the writes
/// become `lane_accepts`, contribute no heat, the chunk cools at the next epoch, and admission
/// normalizes. If they do not, the next patch kills the block, the key re-parks Dormant exactly
/// as before, and the trial cannot re-fire until the epoch turns — worst case one extra compile
/// and install per key per epoch.
/// Whether `imm_lane_for` admits the whole `0x81 /r` reg dword family (`IZARRAVM_LANE_FAMILY`,
/// on for every value except exactly "0") or only the original `/0 ADD` shape. The off arm
/// exists for one-binary A/B measurement, the same contract as the JIT16 pair: both arms ship
/// in one executable so a comparison carries no build-to-build variance.
pub(crate) fn lane_family_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = LANE_FAMILY_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(std::env::var("IZARRAVM_LANE_FAMILY").as_deref(), Ok("0")))
}

// The per-thread arm override, the same shape `IMM8_LANES_OVERRIDE`, `COUNT_LANES_OVERRIDE` and
// `DISP_LANES_OVERRIDE` carry, and added for the same reason: the lane-cap fixtures have to pin
// this knob's arm apart from the shared budget's cap arm, and a process-wide `OnceLock` cannot say
// what an off arm counts without an env write the harness cannot order. `None` leaves every
// existing fixture reading the ambient knob exactly as before.
//
// The knob keeps its bare `!= "0"` reading rather than gaining a spelling table with this
// override: it selects an admission WIDTH inside one family (`/0 ADD` only, against the whole
// `0x81 /r` group) rather than a lane class on or off, no ladder leg outside this crate names it,
// and giving it a table would be an untested behaviour change on a knob nothing was asking about.
#[cfg(test)]
thread_local! {
    static LANE_FAMILY_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the `0x81 /r` family-widening arm on this thread for the length of a fixture; `None`
/// restores the ambient `IZARRAVM_LANE_FAMILY` reading.
#[cfg(test)]
pub(crate) fn set_lane_family_for_test(forced: Option<bool>) {
    LANE_FAMILY_OVERRIDE.with(|cell| cell.set(forced));
}

pub(crate) fn lane_trial_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| !matches!(std::env::var("IZARRAVM_SMC_LANE_TRIAL").as_deref(), Ok("0")))
}

/// Whether `disp_lane_for` admits the `0x8A` displacement-lane family (`IZARRAVM_DISP_LANES`).
/// DEFAULT ON. The off arm exists for one-binary A/B measurement, the same contract as
/// `IZARRAVM_LANE_FAMILY`.
///
/// # THE SPELLING TABLE
///
/// Trimmed and case-folded on the way in, the same table `IZARRAVM_IMM8_LANES` and
/// `IZARRAVM_COUNT_LANES` carry, and it is a table rather than a bare `!= "0"` since the lane-cap
/// fix. The bare form read `IZARRAVM_DISP_LANES=off` as ON: a ladder leg spelling the escape the
/// way every other lane knob accepts it ran the DEFAULT and reported the arm as inert, which is
/// the one wrong conclusion an arm ladder exists to avoid. The Option D ladder reads this leg.
///
/// * **unset** or `1` / `on` -> ON. The shipped default.
/// * `` (empty), `0` or `off` -> OFF. The escape and the A/B base. **The empty string is OFF while
///   unset is ON**, which is the whole family's convention and the reason for it: nulling a
///   variable in PowerShell leaves it PRESENT and EMPTY, so a leg that meant to unset the knob
///   gets the off arm and a leg that meant the off arm gets it too. `Remove-Item Env:` is the only
///   true unset. This is a CHANGE for this knob alone -- the bare form read the empty string as ON
///   -- and it is made deliberately, so that all four lane knobs answer a nulled variable the same
///   way rather than one of them silently disagreeing.
/// * **anything else PANICS**, for `parse_imm8_lanes_arm`'s reason.
pub(crate) fn disp_lanes_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = DISP_LANES_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_disp_lanes_arm(std::env::var("IZARRAVM_DISP_LANES")))
}

/// The `IZARRAVM_DISP_LANES` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `disp_lanes_enabled` for the contract.
fn parse_disp_lanes_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset is the shipped default and the overwhelmingly common case: the displacement lane
        // class is ON, heat-gated by `has_record_range`. `0` / `off` is the escape.
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_DISP_LANES is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default, the `0x8A` \
                 displacement-lane class), and `0` or `off` (the escape, under which every \
                 `0x8A` slot bakes its displacement)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_DISP_LANES={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default, the `0x8A` displacement-lane class), and `0` or `off` \
             (the escape, under which every `0x8A` slot bakes its displacement). Refusing to \
             guess: a mistyped ladder leg would silently run the DEFAULT and be read as the arm \
             it named doing nothing"
        ),
    }
}

// The per-thread arm override, the same shape `IMM8_LANES_OVERRIDE` and `COUNT_LANES_OVERRIDE`
// carry. Added for the lane-cap fixtures, which have to pin the knob arm and the cap arm apart: a
// process-wide `OnceLock` cannot say what an off arm counts without an env write the harness
// cannot order. `None` leaves the existing fixtures reading the ambient knob exactly as before.
#[cfg(test)]
thread_local! {
    static DISP_LANES_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the displacement-lane arm on this thread for the length of a fixture; `None` restores
/// the ambient `IZARRAVM_DISP_LANES` reading.
#[cfg(test)]
pub(crate) fn set_disp_lanes_for_test(forced: Option<bool>) {
    DISP_LANES_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `disp_lanes_enabled` caches its env reading in
/// a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_disp_lanes_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_disp_lanes_arm(value)
}

/// Whether `imm8_lane_for` admits the one-byte `0x80 /r` immediate lane class
/// (`IZARRAVM_IMM8_LANES`). See `imm8_lane_for` for what qualifies and why this family alone.
///
/// **DEFAULT ON SINCE THE 2026-08-21 RE-PRICE.** An unset knob registers one-byte `0x80` lanes;
/// `0` or `off` is the escape back to the pre-slice world, which still ships whole — its baked
/// emitter, its fixtures and its differential sweep — because it is the base every A/B on this
/// class is read against. On the escape no `0x80` lane is registered, every such slot bakes its
/// immediate exactly as it did, and the write choke's per-lane width test degenerates to the old
/// global one because every registered lane is then four bytes wide. Both arms ship in one
/// executable because this box has measured 6% wall variance between builds of identical source
/// (`dev_docs/duke-reprofile-2026-08-19.md` §6.2), so a cross-build comparison would not be
/// evidence.
///
/// # THE REFUTATION INVERTED, exactly as `IZARRAVM_ROTATE_ROWS`'s did before it
///
/// This arm shipped default-OFF on **−1.0%**, a `duke3d-586-short` number taken at `4bf7a4c8` on
/// 2026-08-19. **That number was measured against `IZARRAVM_ROTATE_ROWS=off`, a baseline that
/// stopped existing the same night**, and the same session's nearer pair (`on` -> `on_imm8`, i.e.
/// rotate-rows ON) read **+1.89%** with entries +2.30% — which is why it shipped off. Re-priced
/// against today's full defaults on 2026-08-21 the sign inverted again and hard. This is the second
/// time a duke lane's refutation has reversed once the baseline under it moved; `rotate_rows_enabled`
/// records the first, and the lesson both times is that a lane's value is a property of the
/// baseline, not of the lane.
///
/// **WHAT PRICED THE FLIP** (`.bench/results/imm8-reprice-20260821/`, main `5bec0596`, ONE plain
/// release binary, both arms selected through this knob, `A B B A A B`, pinned CPU 8, every other
/// lane knob exported at its shipped value):
///
/// | row | A (off) min | B (on) min | min-wall | arms |
/// |---|---:|---:|---:|---|
/// | **`duke3d-586` LONG — decides** | 287.423 s | **267.952 s** | **−6.774%** | non-overlapping, 10.1 s clear |
/// | `duke3d-586-short` — corroborates | 127.320 s | **117.615 s** | **−7.623%** | non-overlapping |
///
/// rt on the deciding row **0.4806 -> 0.5155**, the first time duke3d-586 has been above 0.5.
/// DUKEMARK `test_exit:81` with 1027 / 403 samples on every one of the twelve legs, and ONE
/// distinct counter tuple per arm across all twelve, so the wall spread inside an arm is host noise
/// by construction.
///
/// **THE TWO ROWS DISAGREE ON ENTRIES, and that is stated rather than smoothed over.**
/// `jit_direct_entries` is **−7.05% on the long row and +1.99% on the short one**
/// (`jit_direct_blocks_installed` −7.36% against +4.56%: the two rows settle on different block
/// populations). The 2026-08-21 `0x85` adjudication named entries as duke's acceptance metric in
/// place of coverage, and on this slice the deciding row honours that and the corroborating row
/// does not. **The metric that IS consistent across both rows, and with the wall, is total
/// dispatcher asks — `entries + declines`, −18.8% long and −17.0% short** — with declines alone
/// −22.66% / −22.46%, coverage +3.78 pp / +4.16 pp, `jit_direct_insns` +4.68% / +5.29%, and
/// block-killing `code_invalidations` −19.67% / −22.12%. Price asks in the next duke
/// pre-registration.
///
/// **THE PRE-REGISTERED ENGAGEMENT FALSIFIER WAS MIS-SPECIFIED and is recorded as such, not as
/// evidence.** It said `smc_lane_registrations` must RISE on the ON arm. It fell 13.57% on the long
/// row and rose 1.91% on the short one — because it is not `0x80`-specific (it counts every lane
/// class) and it is a PER-COMPILE event counter, so it tracked `blocks_installed` in both
/// directions rather than tracking the lane. Engagement is established instead by counters the gate
/// is the only possible cause of, the arms being one binary differing in one environment variable:
/// `smc_lane_accepts` +19.9% per registration, and ~9.8 M block-killing invalidations that stop
/// happening (`code_invalidations` and `smc_narrow_kills` both ~−20%), which is what a laned write
/// not killing a block looks like. `smc_imm8_lane_registrations` DOES ship in plain builds — it
/// rides `DirectStallSnapshot` into `direct_stall_json` ungated (the flip doc first called it
/// census-only, which the heat-gate design review refuted at the line). What a plain build still
/// lacks is the `0x80`-specific runtime ACCEPT counter; closing that is the owed follow-up.
///
/// # THE SPELLING TABLE
///
/// Trimmed and case-folded on the way in, because a knob set from a shell script picks up
/// whitespace and one set from a PowerShell ladder picks up capitalisation.
///
/// * **unset** or `1` / `on` -> ON. The shipped default since the 2026-08-21 flip. Every "defaults"
///   leg recorded BEFORE that date is the OFF arm and is not comparable with one recorded after.
/// * `` (empty), `0` or `off` -> OFF. The escape, the pre-slice base and the A/B base.
///   **The empty string is OFF while unset is ON, and the two must not be confused**: nulling a
///   variable in PowerShell leaves it PRESENT and EMPTY, which is how three earlier evidence
///   directories came to run their default-ON knobs off. `Remove-Item Env:` is the only true unset.
/// * **anything else PANICS**, for `parse_rotate_rows_arm`'s reason restated in this arm's terms: a
///   mistyped ladder leg (`IZARRAVM_IMM8_LANES=yes`, `=imm8`, `=true`) that fell through would now
///   silently run the DEFAULT and be read as "the arm I asked for changed nothing", which is the
///   one wrong conclusion an arm ladder exists to avoid. Failing at the first compile is cheaper
///   than a plausible number.
pub(crate) fn imm8_lanes_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = IMM8_LANES_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_imm8_lanes_arm(std::env::var("IZARRAVM_IMM8_LANES")))
}

/// The `IZARRAVM_IMM8_LANES` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `imm8_lanes_enabled` for the contract.
fn parse_imm8_lanes_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset = ON since the 2026-08-21 flip; `0` / `off` is the escape. Same shape and the same
        // trap as `IZARRAVM_ROTATE_ROWS`, `IZARRAVM_COUNT_LANES`, `IZARRAVM_FPU_LOOP_ROWS` and
        // `IZARRAVM_V86_LOOP_ROWS`: an off leg must EXPORT `0`, and every "defaults" leg recorded
        // before that date is the OFF arm. NULLING the variable is not unsetting it -- PowerShell
        // leaves it present and empty, and the empty string is spelled OFF one arm down.
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_IMM8_LANES is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default: the one-byte `0x80` \
                 immediate lane class), and `0` / `off` (the escape, under which every `0x80` \
                 slot bakes its immediate)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_IMM8_LANES={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default: the one-byte `0x80` immediate lane class is registered), \
             and `0` / `off` (the escape, under which every `0x80` slot bakes its immediate). \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be read \
             as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `ROTATE_ROWS_OVERRIDE`'s reason: the shipped knob is a process-wide `OnceLock`
// and the fixtures have to run both arms in one process, so one test's arm selection must not
// reach another's compile. THE DIRECTION THAT MATTERS FLIPPED WITH THE DEFAULT ON 2026-08-21: it
// used to be the positive fixtures that had to force this on, and it is now the REFUSAL fixtures
// that must force `Some(false)`, because one that read the ambient arm would register a lane and
// pass for the wrong reason. Two fixtures outside this file learned that the hard way on the flip
// (`generated_direct_blocks_match_interpreter_in_486_and_586_modes` and
// `near_miss_shapes_take_no_count_lane`); see `cpu_test.rs`'s `DIRECT_BARRIER` doc for the rule.
#[cfg(test)]
thread_local! {
    static IMM8_LANES_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force the one-byte lane arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_IMM8_LANES` reading.
#[cfg(test)]
pub(crate) fn set_imm8_lanes_for_test(forced: Option<bool>) {
    IMM8_LANES_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `imm8_lanes_enabled` caches its env reading in
/// a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_imm8_lanes_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_imm8_lanes_arm(value)
}

/// Whether `count_lane_for` admits the one-byte GROUP-2 COUNT lane class
/// (`IZARRAVM_COUNT_LANES`). See `count_lane_for` for what qualifies, and `emit_rotate_reg_lane` /
/// `emit_shift_lane` for the runtime three-way branch that is the whole cost of the class.
///
/// **DEFAULT ON SINCE THE 2026-08-20 LADDER.** An unset knob registers count lanes; `0` or `off`
/// is the escape back to the pre-slice world, which still ships whole -- its baked emitters, its
/// fixtures and its mutation record -- because it is the base every A/B on this class is read
/// against. On the escape no lane is registered, every `0xC1` and `0xC0` slot bakes its count
/// exactly as it did, and the emitted code is the compile-time three-way split it has always been.
/// Both arms ship in one executable because this box has measured 6% wall variance between builds
/// of identical source (`dev_docs/duke-reprofile-2026-08-19.md` §6.2), so a cross-build comparison
/// would not be evidence.
///
/// **WHY IT IS ON (2026-08-20).** `.bench/results/duke-l2-count-lane-20260820/`, one binary at
/// `a0912841`, both arms selected through this knob, pinned CPU 8, quiet host, DUKEMARK stop
/// `test_exit:81` on every leg, per-arm counters deterministic across legs:
///
/// * duke3d-586-**short**: base min 142.64 s, count arm min 134.47 s over three interleaved legs
///   each. **-5.73%**, and the C1 invariants held on every leg.
/// * duke3d-586 **long merge-gate row**: base min 314.62 s, count arm min 299.07 s. **-4.94%
///   min-wall**, rt 0.44 -> 0.46, native coverage 0.78 -> 0.83.
///
/// THE MECHANISM IS THE ONE THE SLICE WAS BUILT FOR, and the counters say so directly:
/// `smc_lane_accepts` 91.8 M -> 113.4 M on the long row, i.e. the count-byte patches that used to
/// kill blocks are now absorbed, and `smc_heat_demotions` fell 52%. The census legs close exactly:
/// `dormant_probe` declines -16.7%, `dormant_heat` -18.4% (66 -> 47 sites), dormant SUM -5.8% with
/// no full re-attribution this time, and `rejected` flat.
///
/// **TWO COUNTERS MOVE AGAINST THE GRAIN and a future reader should not be surprised by them:
/// static-unbound rose +9.6% and narrow kills +4.4% on the count arm of the LONG WALL ROW (the
/// short row and the census leg both read static-unbound -4.3%, so the sign is row-specific).**
/// Both are the expected
/// shape of the win rather than a contradiction of it: absorbing the count patches keeps blocks
/// alive, so spans that used to end at a patched byte now extend past it and reach NEW seams, and
/// the newly admitted spans bring their own decode lines for the interpreter to kill. The wall
/// wins anyway, on both rows, which is the currency this ladder is read in. If a later slice moves
/// these two counters the other way, that is not automatically progress -- check the wall.
///
/// **A SEPARATE KNOB FROM `IZARRAVM_IMM8_LANES`, deliberately, and NOT because the two classes are
/// unrelated.** They are both one-byte lanes and they share the width class, the budget and the
/// write choke. What forces them apart is the LADDER: `rotate_rows_enabled`'s cross-term paragraph
/// shows that one-byte lanes interact with group-2 admission through the per-chunk heat map, so a
/// combined knob would make the arm-1 and arm-2 deltas unrecoverable from each other. Two knobs
/// give the 2x2 four legs that can actually be measured.
///
/// **THE SPELLING TABLE.** Trimmed and case-folded on the way in, because a knob set from a shell
/// script picks up whitespace and one set from a PowerShell ladder picks up capitalisation.
///
/// * **unset**, `1` or `on` -> ON. **The shipped default since 2026-08-20.** `1` is pinned to this
///   arm rather than merely accepted by it, because `1` is the spelling every ladder leg in
///   `.bench/results/duke-l2-count-lane-20260820/` used, and keeping it stable is what makes those
///   legs comparable with a leg run on a later binary.
/// * `` (empty), `0` or `off` -> OFF. The pre-slice world, and **the escape and the base** every
///   A/B on this class is read against. Empty stays here with `0` and `off` rather than following
///   "unset" to ON, for `parse_rotate_rows_arm`'s reason: a wrapper script that computes the value
///   and produces "" meant something falsy, and `IZARRAVM_COUNT_LANES=` is the shell's shortest
///   way to say off.
/// * **anything else PANICS**, for `parse_rotate_rows_arm`'s reason, and the reason is STRONGER
///   now that the fallthrough arm would be the default: a mistyped leg (`IZARRAVM_COUNT_LANES=yes`,
///   `=count`, `=true`) that fell through would run exactly what an unset environment runs and be
///   read as "the arm I asked for changed nothing", which is the one wrong conclusion an arm ladder
///   exists to avoid.
pub(crate) fn count_lanes_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = COUNT_LANES_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_count_lanes_arm(std::env::var("IZARRAVM_COUNT_LANES")))
}

/// The `IZARRAVM_COUNT_LANES` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `count_lanes_enabled` for the contract.
fn parse_count_lanes_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset is the shipped default and the overwhelmingly common case. Since the 2026-08-20
        // ladder that default is ON (-5.73% short, -4.94% long; see `count_lanes_enabled`).
        // `0` / `off` is the escape back to the pre-slice world.
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_COUNT_LANES is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default, the group-2 count-byte \
                 lane class), and `0` or `off` (the pre-slice world, the escape)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_COUNT_LANES={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default since 2026-08-20, the group-2 count-byte lane class), and \
             `0` or `off` (the pre-slice world, the escape). Refusing to guess: a mistyped ladder \
             leg would silently run the DEFAULT and be read as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `IMM8_LANES_OVERRIDE`'s reason: the shipped knob is a process-wide `OnceLock`
// and the fixtures have to run both arms in one process, so one test's arm selection must not
// reach another's compile.
//
// Not a convenience, in EITHER direction, and the direction that matters flipped on 2026-08-20.
// While the arm was default-OFF, every positive fixture for this class had to force it on through
// here or it would test the refusal and call it a lowering. Now that the default is ON, it is the
// REFUSAL fixtures that must force `Some(false)` explicitly -- a fixture that means to pin the
// baked-count world and reads the ambient arm would silently compile a lane and pass for the wrong
// reason. Both kinds state their arm; neither leans on the default.
#[cfg(test)]
thread_local! {
    static COUNT_LANES_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the count-lane arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_COUNT_LANES` reading.
#[cfg(test)]
pub(crate) fn set_count_lanes_for_test(forced: Option<bool>) {
    COUNT_LANES_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `count_lanes_enabled` caches its env reading in
/// a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_count_lanes_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_count_lanes_arm(value)
}

/// Whether `classify` admits the FIVE dword rows the 32-bit FPU-loop census names
/// (`IZARRAVM_FPU_LOOP_ROWS`). **DEFAULT ON since 2026-08-20** (the slice shipped OFF for one
/// commit; the flip was priced by the tombraid-586 wall ladder exactly as `IZARRAVM_ROTATE_ROWS`
/// and `IZARRAVM_COUNT_LANES` were -- the numbers are in the spelling table below).
///
/// The rows, and each one's home:
///
/// | row | form | where it lowers |
/// |---|---|---|
/// | `0x9B` WAIT/FWAIT | no operand | `NativeX87Insn::Wait`, an x87 slot whose whole body is the gate |
/// | `0x9E` SAHF | no operand | `DirectKind::Sahf` |
/// | `0xD9 /7` mod=3 rm=4 FRNDINT | register | `NativeX87Insn::RoundToInt` |
/// | `0xF7 /6`,`/7` DIV/IDIV r/m32 | MEMORY | `DirectKind::DivMem` |
/// | `0x0F90..=0x0F9F` SETcc r/m8 | MEMORY | `DirectKind::SetCcMem` |
///
/// WHY IT EXISTS. `dev_docs/tombraid-reprofile-2026-08-20.md` §4: on tombraid-586 the FMV phase is
/// 77% of the row's wall at rt 0.609, 93.6% of its JIT entries end on a `static_unbound` exit, and
/// 72.0% of that mass (643.5 M of 893.3 M) is class `rejected` -- the successor block is REFUSED
/// compilation. `.bench/results/tombraid-reprofile-20260820/census-fmv-summary.json` splits the
/// rejected table into two loops, and this knob is the whole of loop B, the game's 32-bit FPU loop
/// at linear `0x49ACF3-0x4CB300` (mode key `0x31B`, ~27.6 M iterations). Its five rows, by
/// interpreted runtime hits over the 20e9 census prefix:
///
/// * `0x9B` **165,587,061** -- three sites, six per iteration, the single largest row in the table;
/// * `0x9E` 55,203,044 and `0xD9 /7` 55,195,911 -- two per iteration each;
/// * `0xF7 /7` memory 27,602,949 and `0x0F94 /0` memory 27,602,402 -- one per iteration each.
///
/// **THE DOC'S NAME FOR THE THIRD ROW IS WRONG, and the census says so on its face.** Both
/// `dev_docs/tombraid-reprofile-2026-08-20.md` §4.1 and the handoff call `0xD9 /7` "FNSTCW". The
/// row's `operand_form` is `"none"`, and `decode`'s FPU arm sets `insn.operand` for `mod != 3`
/// ONLY -- so the row is a REGISTER form, and FNSTCW m16 (which is `mod != 3`, and which
/// `NativeX87Insn::StoreControlWord` has lowered since before this slice) cannot be it. The eight
/// `mod = 3` encodings are FPREM/FYL2XP1/FSQRT/FSINCOS/FRNDINT/FSCALE/FSIN/FCOS. Which one was
/// settled by measurement rather than by inference: a temporary probe in `execute_fpu_register`,
/// run on the tombraid fixture at 4e9 cycles, saw ModRM byte `0xFC` and nothing else, i.e.
/// **FRNDINT**. That matters beyond bookkeeping -- FNSTCW would have been a two-byte store and
/// FSIN/FCOS/FPTAN are not expressible in SSE at all, so the row's identity decides whether the
/// slice is buildable.
///
/// THE SPELLING TABLE, trimmed and case-folded on the way in, matching `IZARRAVM_COUNT_LANES`:
///
/// * **unset** or `1` / `on` -> ON. The shipped default since 2026-08-20: the tombraid-586
///   ladder read -17.2% min-wall (211.5 vs 255.4 s, three legs per arm, full non-overlap,
///   `.bench/results/tomb-fmv-admission-20260819/wall-ladder/`), guest counters byte-identical
///   per arm. Recorded before that date, an "on" leg is the non-default arm.
/// * `` (empty), `0` or `off` -> OFF. The escape, the pre-slice refusal and the A/B base.
/// * **anything else PANICS**, for `parse_rotate_rows_arm`'s reason: a mistyped ladder leg that
///   fell through to the default would be read as "the arm I asked for changed nothing".
pub(crate) fn fpu_loop_rows_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = FPU_LOOP_ROWS_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_fpu_loop_rows_arm(std::env::var("IZARRAVM_FPU_LOOP_ROWS")))
}

/// The `IZARRAVM_FPU_LOOP_ROWS` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `fpu_loop_rows_enabled` for the contract.
fn parse_fpu_loop_rows_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset = ON since the 2026-08-20 flip; `0` / `off` is the escape. Same shape (and same
        // trap) as `IZARRAVM_COUNT_LANES`: an off leg must EXPORT `0`, not unset the variable.
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_FPU_LOOP_ROWS is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default: the five 32-bit \
                 FPU-loop rows), and `0` / `off` (the escape, the pre-slice refusal)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_FPU_LOOP_ROWS={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default: the five 32-bit FPU-loop rows: WAIT, SAHF, FRNDINT, \
             DIV/IDIV memory and SETcc memory), and `0` / `off` (the escape, the pre-slice \
             refusal and the A/B base). Refusing to guess: a mistyped ladder leg would silently \
             run the DEFAULT and be read as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `IMM8_LANES_OVERRIDE`'s reason: the shipped knob is a process-wide `OnceLock`
// and the fixtures have to run both arms in one process, so one test's arm selection must not
// reach another's compile. Every fixture for these five rows states its arm through here in
// BOTH directions -- a positive fixture that rode the ambient default would go vacuous the day
// the default moved, and the negative (refusal) fixtures need the off arm now that ON ships.
#[cfg(test)]
thread_local! {
    static FPU_LOOP_ROWS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the FPU-loop-row arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_FPU_LOOP_ROWS` reading.
#[cfg(test)]
pub(crate) fn set_fpu_loop_rows_for_test(forced: Option<bool>) {
    FPU_LOOP_ROWS_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `fpu_loop_rows_enabled` caches its env reading
/// in a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process
/// and never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_fpu_loop_rows_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_fpu_loop_rows_arm(value)
}

/// Whether the backend admits the SIX rows the tombraid FMV census's loop A names, plus the two
/// collateral forms the same code path needs (`IZARRAVM_V86_LOOP_ROWS`). **DEFAULT ON since
/// 2026-08-20** (the slice shipped OFF for three commits; the flip is priced in the spelling table
/// below).
///
/// The rows, and each one's home:
///
/// | row | form | where it lowers |
/// |---|---|---|
/// | `0x07` POP ES (97.3M hits) and `0x1f` POP DS (6.0M) | no operand | `DirectKind::PopSegReal` |
/// | `0x3d` CMP AX,imm16 (ALU form 5 at Word) | no operand | `DirectKind::AluImm` |
/// | `0xa1` MOV AX,moffs16 | no operand | `DirectKind::Load` at `MemoryWidth::Word` |
/// | `0xf8` CLC, `0xf9` STC | no operand | `DirectKind::CarryFlag` |
/// | `0xff /1` DEC m16 with a CS override | memory | `DirectKind::RmwIncDec`, already lowered |
/// | `0x2b /0` SUB r16,m16 with a CS override | memory | `DirectKind::AluMemSource`, already lowered |
///
/// plus `0xa3` MOV moffs16,AX and `0xc7 /0` MOV m16,imm16 with a CS override, which the same
/// straight-line run passes through and which the census measures as rows of their own.
///
/// WHY IT EXISTS. `dev_docs/tombraid-reprofile-2026-08-20.md` §4.1: after the FPU-loop slice
/// (`IZARRAVM_FPU_LOOP_ROWS`) closed loop B, the tombraid-586 FMV window's remaining `rejected`
/// mass is loop A, a V86 16-bit driver loop at linear `0xC8FDE-0xC9035` (UMB, mode key `0x316`,
/// ~93M iterations). A decoder probe over the fixture disassembles it as a BIOS-tick watchdog that
/// keeps its counters in its OWN code segment:
///
/// ```text
/// 0xc9008 06                    push es
/// 0xc9009 b8 40 00              mov ax, 0x40
/// 0xc900c 8e c0                 mov es, ax
/// 0xc900e 26 a1 6c 00           mov ax, es:[0x6c]   ; BIOS tick 0040:006C
/// 0xc9012 2e 2b 06 f5 00        sub ax, cs:[0xf5]   ; -> linear 0xc8115
/// 0xc9017 3d b6 00              cmp ax, 0xb6
/// 0xc901a 73 1b                 jnb
/// 0xc901c 2e ff 0e f3 00        dec word cs:[0xf3]  ; -> linear 0xc8113
/// 0xc9021 75 0e                 jne
/// 0xc9023 2e c7 06 f3 00 ff ff  mov word cs:[0xf3], 0xffff
/// 0xc902a 2e ff 0e f1 00        dec word cs:[0xf1]  ; -> linear 0xc8111
/// 0xc902f 74 06                 je
/// 0xc9031 07                    pop es
/// 0xc9032 59 5b 58              pop cx / pop bx / pop ax
/// 0xc9035 f8                    clc
/// 0xc9036 c3                    ret
/// ```
///
/// **THE RE-PROFILE MISNAMED THE TWO `prefix_unsupported` ROWS' SEGMENT.** §4.1 and the evening
/// handoff both call them "word-memory forms (both prefix mask 64)" and the queue item calls the
/// refusal "the segment-override prefix", which reads as one of the five DATA segments -- the class
/// `prefixes_supported_for` has admitted since the rejected-row campaign's slice 6. Mask 64 is
/// **CS**: `BarrierShape::from_insn` writes `(segment_index(seg) + 1) << 5`, and
/// `segment_index(Cs)` is 1. ES would be 32, SS 96, DS 128. That was settled by measurement rather
/// than by arithmetic -- a temporary probe printed the decoder's own
/// `insn.prefixes.segment_override` for every instruction in the window, and every mask-64 row came
/// back `Cs`. It matters because CS was refused DELIBERATELY, with a written justification on
/// `prefixes_supported_for`, so this is a reversal rather than a widening.
///
/// The rows' interpreted runtime hits over the 20e9 census prefix, and their static unbound exits:
///
/// * `0x07` 97,347,816 hits / 95,057,524 exits;
/// * `0x3d` 96,182,170 / 587,085;
/// * `0xa1` 95,614,884 / 95,502,528;
/// * `0xf8` 95,090,745 / 94,883,220;
/// * `0xff /1` cs: 95,055,642 / 95,020,029;
/// * `0x2b /0` cs: 95,055,326 / 0.
///
/// THE ROWS LAND TOGETHER OR NOT AT ALL, and the mechanism is per BLOCK rather than one long walk.
/// `DirectKind::Jcc` is terminal, so the loop is four compile units, and the barrier census records
/// the FIRST instruction that stops each one:
///
/// * `0xc900e`: `0xa1` -> `0x2b cs:` -> `0x3d` -> `jnb`. It stops at `0xa1` today, and each
///   admission moves the stop one instruction along, so all three are needed for the unit to exist.
/// * `0xc901c`: `0xff /1 cs:` -> `jne`, two slots with a terminal last, which the walk's
///   fewer-than-three rule admits. See the CERTAIN-EXIT rule in the compile walk for why this one
///   stays a barrier anyway.
/// * `0xc9023`: `0xc7 /0 cs:` -> `0xff /1 cs:` -> `je`, the same.
/// * `0xc9031`: `0x07` -> three `Pop16` -> `0xf8`, stopping before the `ret` on
///   `MAX_BLOCK_STACK_ACCESSES`.
///
/// The census numbers say the same thing: `0x2b cs:` carries ZERO unbound exits and `0x3d` carries
/// 587,085, because both are interior to the `0xc900e` unit, while `0x07`, `0xa1`, `0xf8` and
/// `0xff /1` each carry ~95M because each is its own entry target. A proper subset relocates the
/// stop onto the next member and buys nothing, which is the census relocation trap in its exact
/// local form.
///
/// THE SPELLING TABLE, trimmed and case-folded on the way in, matching `IZARRAVM_FPU_LOOP_ROWS`
/// exactly, default included since the 2026-08-20 flip:
///
/// * **unset** or `1` / `on` -> ON. The shipped default. Recorded before that date, an "on" leg is
///   the non-default arm.
/// * `` (empty), `0` or `off` -> OFF. The escape, the pre-slice refusal and the A/B base.
/// * **anything else PANICS**, for `parse_rotate_rows_arm`'s reason: a mistyped ladder leg that
///   fell through to the default would be read as "the arm I asked for changed nothing".
///
/// WHAT PRICED THE FLIP, in the order the evidence was taken
/// (`.bench/results/tomb-v86-loop-20260820/`):
///
/// * **tombraid-586 wall ladder**, one binary, A B B A A B, full 28e9 row: **-13.43% min-wall**
///   (180.500 s against 208.498 s), arms fully non-overlapping, row rt 0.8100 -> 0.9349, 16-bit
///   insns/entry 3.568 -> 5.607.
/// * **Census closure**, 20e9 boot+FMV prefix: the gate-OFF arm is byte-identical to MAIN's own
///   rebuilt binary on twelve counters, and the ON arm's `rejected` class falls 299,371,338 with
///   the rows' own mass reconciling to a residual of **exactly zero**.
/// * **doom-486**, the protected-mode CS-override READ half the tombraid ladder cannot price:
///   `0xFF /4 jmp dword [cs:m]` is lowered, `jit_direct_exit_cross_page_or_alignment` moves by
///   **0**, and the guest oracle is unchanged at 2134 gametics in 2883 realtics.
/// * **Board leg, gate ON**: 12 of 12 fixtures pass against main's own pins.
/// * **wolf3d-586**, the one fixture whose mechanism counters moved hard: **NEUTRAL** over twelve
///   legs across two ladders of opposite order (median +0.008%), and its row-level census shows
///   every departing shape inside this gate's named population with a zero reconciliation
///   residual. Its inertness is RELOCATION, not inaction: the admitted rows convert and the blocks
///   stop one or two instructions later on `0x01 /0` ADD word memory, `0x8E /0`, `0x61` POPA and
///   `0xF7 /7` word, every one a documented refusal. That is the next slice for wolf3d, not a
///   defect in this one.
pub(crate) fn v86_loop_rows_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = V86_LOOP_ROWS_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_v86_loop_rows_arm(std::env::var("IZARRAVM_V86_LOOP_ROWS")))
}

/// The `IZARRAVM_V86_LOOP_ROWS` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `v86_loop_rows_enabled` for the contract.
fn parse_v86_loop_rows_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset = ON since the 2026-08-20 flip; `0` / `off` is the escape. Same shape and the
        // same trap as `IZARRAVM_FPU_LOOP_ROWS`, `IZARRAVM_ROTATE_ROWS` and `IZARRAVM_COUNT_LANES`:
        // an off leg must EXPORT `0`, and every "defaults" leg recorded BEFORE this flip is the
        // OFF arm. NULLING the variable is not unsetting it -- PowerShell leaves it present and
        // empty, and the empty string is spelled OFF two arms down, which is how three earlier
        // evidence directories came to measure their default-ON knobs off.
        Err(std::env::VarError::NotPresent) => return true,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_V86_LOOP_ROWS is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default: the six 16-bit \
                 V86-loop rows and the CS-override clause), and `0` / `off` (the escape, under \
                 which every one of them stays a barrier)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_V86_LOOP_ROWS={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default: POP ES/DS, CMP AX imm16, MOV AX moffs16, CLC/STC and the \
             CS-override word-memory forms are all lowered), and `0` / `off` (the escape, under \
             which every one of them stays a barrier). \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be read \
             as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `FPU_LOOP_ROWS_OVERRIDE`'s reason: the shipped knob is a process-wide `OnceLock`
// and the fixtures have to run both arms in one process. Every fixture for these rows states its
// arm through here in BOTH directions -- the refusal fixtures need the off arm stated rather than
// inherited, so that they keep meaning what they say the day the default moves.
#[cfg(test)]
thread_local! {
    static V86_LOOP_ROWS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the V86-loop-row arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_V86_LOOP_ROWS` reading.
#[cfg(test)]
pub(crate) fn set_v86_loop_rows_for_test(forced: Option<bool>) {
    V86_LOOP_ROWS_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `v86_loop_rows_enabled` caches its env reading
/// in a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process
/// and never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_v86_loop_rows_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_v86_loop_rows_arm(value)
}

/// Whether `classify` admits **`0x85` TEST r/m16, r16, REGISTER form, at Word operand size**
/// (`IZARRAVM_TEST_WORD_ROWS`).
///
/// **DEFAULT OFF.** An unset knob is the pre-slice refusal and the A/B base, the same contract
/// `IZARRAVM_IMM8_LANES` ships under and the OPPOSITE of `IZARRAVM_V86_LOOP_ROWS` /
/// `IZARRAVM_ROTATE_ROWS` / `IZARRAVM_COUNT_LANES` / `IZARRAVM_FPU_LOOP_ROWS`, whose unset arm is
/// the slice. Both arms ship in one executable because this box has measured 6% wall variance
/// between builds of byte-identical source (`dev_docs/duke-reprofile-2026-08-19.md` §6.2), so a
/// cross-build comparison would not be evidence.
///
/// # The row, from the census that ranked it
///
/// `.bench/results/duke-census-slice-20260821/`, duke3d-586 LONG row at main `49c7ad97` — the
/// first duke census taken after `IZARRAVM_ROTATE_ROWS`, `IZARRAVM_COUNT_LANES`,
/// `IZARRAVM_FPU_LOOP_ROWS` and `IZARRAVM_V86_LOOP_ROWS` all became the shipped default. Ranked by
/// `runtime_hits` per [[barrier-census-mispredicts-both-ways]], the whole rejected table is
/// 126,933,336 runtime hits over 287 rows, and **`0x85` register-form Word is 53,583,389 of them
/// -- 42.2%, over twelve rows, 2.4x the next single row.** It is the head either way you count:
/// the largest of the twelve is 25,275,365 on its own, which is still the table's #1 against
/// `0x8E /0`'s 22,638,814.
///
/// | `/reg` | asize | pfx | `runtime_hits` | `unbound_exits` | mean suffix | max suffix |
/// |---|---|---|---:|---:|---:|---:|
/// | 1 | dword | 0x66 | 25,275,365 | 25,201,852 | 0.94 | 1 |
/// | 1 | word | — | 8,520,955 | 8,430,959 | 1.00 | 1 |
/// | 2 | word | — | 7,053,194 | 11,527 | 1.00 | 1 |
/// | 6 | dword | 0x66 | 4,685,484 | 4,381,759 | 1.00 | 1 |
/// | 0 | dword | 0x66 | 4,203,893 | 1,452,445 | 0.99 | 3 |
/// | 3 | dword | 0x66 | 1,778,127 | 1,666,320 | 1.00 | 1 |
/// | 7 | dword | 0x66 | 1,251,170 | 981,894 | 1.00 | 1 |
/// | 2 | dword | 0x66 | 646,191 | 493,959 | 1.00 | 1 |
/// | 0/7/6/3 | word | — | 169,010 | 22,059 | 1.00 | 1 |
///
/// `modrm_reg` is a register NUMBER on this opcode and not a group extension, which is why the row
/// is twelve rows rather than one: TEST r/m, r has no `/digit` form. All twelve are one shape.
///
/// # Why it clears the standing bar on further Word lowering, which is the only reason it ships
///
/// The re-profile's "NOT levers on this row" section closes further Track-A Word lowering behind a
/// rule rather than a verdict: **S1 was implemented and REFUTED on wall (-3.0%) by extension
/// exposure into patched spans, and extension exposure must be PRICED before any further Word
/// row is admitted.** This row is priced by the census's own suffix columns and it is the
/// cheapest possible answer: **`max_native_suffix` is 1 on eleven of the twelve rows and 3 on the
/// twelfth**, with a mean of ~1.00. The counterfactual block grows by the TEST plus exactly one
/// instruction and then stops, so the span this admission exposes is bounded at one instruction by
/// measurement rather than by hope. Compare the rows this reasoning REFUSES in the same table:
/// `0xD3 /4` memory dword carries a mean suffix of 26.88 (max 31) and `0xF6 /3` register 10.49.
/// Those are extension-exposure rows; this one is not.
///
/// The one instruction is almost always the paired `Jcc`, which is terminal, so what the admission
/// buys is not a longer block so much as a block that ENDS AT A BRANCH instead of at an unbound
/// exit. Predict `jit_direct_linked_transfers` up and `jit_direct_unresolved_static_unbound` down
/// by roughly the row's own 42.6M exits.
///
/// # What is deliberately NOT in this gate
///
/// * **The MEMORY form of `0x85`, at either width.** It has no arm today and the duke census
///   measures **zero** memory rows for this opcode, so admitting it would be a formation change
///   with no row to attribute it to.
/// * **`0xA9` TEST eAX, imm at Word.** Its kind (`TestImmReg`) has carried a width since it was
///   written and `emit_test_imm_reg` is fully width-parameterised, so it is one allowlist entry
///   away — and it stays out anyway, because the duke census measures **zero** `0xA9` rows at any
///   width. Riding it along would be an unmeasured admission, which is the campaign's standing
///   refusal. It is a one-line follow-up for whichever census first ranks it.
/// * **`0x8E /0` MOV Sreg, m16, the table's #2 row at 22,638,814 hits (17.8%).** Refused on duke's
///   own law rather than on difficulty: its mean suffix is **0.31**. It TERMINATES rather than
///   EXTENDS — and it must, because a segment write makes its block a segment-write block, which
///   bars the self-loop shape and attempts no static link. Converting it would move ~1.3
///   instructions per hit into native code and buy nothing on the link axis.
///
/// # The spelling table
///
/// Trimmed and case-folded on the way in, because a knob set from a shell script picks up
/// whitespace and one set from a PowerShell ladder picks up capitalisation.
///
/// * unset, `` (empty), `0` or `off` -> OFF. The shipped base.
/// * `1` or `on` -> ON. The slice.
/// * **anything else PANICS**, for `parse_rotate_rows_arm`'s reason in this slice's terms: a
///   mistyped ladder leg (`=yes`, `=test`, `=true`) that fell through to OFF would run the BASE
///   and be read as "the TEST word row did nothing", which is the one wrong conclusion this slice
///   exists to avoid.
pub(crate) fn test_word_rows_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = TEST_WORD_ROWS_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_test_word_rows_arm(std::env::var("IZARRAVM_TEST_WORD_ROWS")))
}

/// The `IZARRAVM_TEST_WORD_ROWS` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `test_word_rows_enabled` for the contract.
fn parse_test_word_rows_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return false,
        // Not-UTF-8 is not a spelling of either arm. It reaches the same panic as a typo rather
        // than the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_TEST_WORD_ROWS is set to a value that is not valid UTF-8; accepted \
                 spellings are unset, `0` or `off` (the shipped base, under which `0x85` at Word \
                 stays a barrier), and `1` or `on` (the Word register-form TEST admission)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_TEST_WORD_ROWS={other:?} names no arm; accepted spellings are unset, `0` or \
             `off` (the shipped base) and `1` or `on` (the `0x85` TEST r/m16,r16 register-form \
             admission at Word operand size). \
             Refusing to guess: a mistyped ladder leg that silently ran the base would be read as \
             the slice failing"
        ),
    }
}

// Per-THREAD, for `ROTATE_ROWS_OVERRIDE`'s reason: the shipped knob is a process-wide `OnceLock`
// and the fixtures have to run both arms in one process, so one test's arm selection must not
// reach another's compile. Since the arm is default-OFF, every positive fixture for this row MUST
// force it on through here or it would test the refusal and call it a lowering; and every refusal
// fixture states the off arm rather than inheriting it, so that it keeps meaning what it says the
// day the default moves.
#[cfg(test)]
thread_local! {
    static TEST_WORD_ROWS_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the Word-TEST arm on this thread for the length of a fixture; `None` restores the ambient
/// `IZARRAVM_TEST_WORD_ROWS` reading.
#[cfg(test)]
pub(crate) fn set_test_word_rows_for_test(forced: Option<bool>) {
    TEST_WORD_ROWS_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `test_word_rows_enabled` caches its env
/// reading in a process-wide `OnceLock`, so the contract is otherwise assertable exactly once per
/// process and never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_test_word_rows_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_test_word_rows_arm(value)
}

/// Whether `classify` admits the 2026-08-09 group-2 rows -- `0xC1`/`0xD1` **`/0` ROL** and
/// **`0xC0 /4` SHL r8**, both register forms.
///
/// **DEFAULT ON SINCE THE 2026-08-19/20 RE-MEASUREMENT.** `IZARRAVM_ROTATE_ROWS` unset admits both
/// rows unconditionally; `0` or `off` is the escape back to the pre-slice refusal, which still
/// ships whole -- its classify path, its fixtures and its mutation record -- because it is the base
/// every A/B on these rows is read against.
///
/// WHY IT EXISTS. The duke3d-586 re-census (`.bench/results/duke586-census-20260809.json`) ranks
/// `0xC1 /0` first by BOTH currencies -- 260,659,304 runtime hits, the hottest interpreted
/// instruction in the trace, and 111,123,374 static unbound exits, the largest refused-row seam --
/// with `0xC0 /4` second at 32,839,852 and 31,743,121. On paper this is the top of the list.
///
/// WHY IT WAS OFF (2026-08-09). Interleaved A/B/B/A on duke3d-586, one binary, quiet box. Off arm
/// rt 0.3298 (legs 0.3283, 0.3313); on arm rt 0.3184 (legs 0.3111, 0.3257). **Delta -3.44%**, and
/// native coverage DROPPED on the admitting arm, 0.7480 -> 0.7264. Admitting the hottest row made
/// the backend cover LESS, so the row shipped refused.
///
/// THE MECHANISM THAT DAY, which the counters named outright: `smc_lane_accepts` collapsed
/// 55.57M -> 25.45M, narrow kills rose 45.25M -> 49.55M, `heat_hot` 357k -> 373k. Duke patches the
/// COUNT BYTE of its group-2 shifts (the SMC shape table's `0xC1 /0,/4,/5` `imm_len=1` rows, ~1.9M
/// events). Before the slice those ROLs were hard boundaries, so no compiled block ever spanned the
/// patched byte and the patch cost nothing. After it, blocks span the byte -- and each 1-byte count
/// patch now kills a block that ALSO carries live `0x81` imm lanes and `0x8A` displacement lanes,
/// taking their accepts down with it. That is the lane-trial iteration-1 mixing failure reborn one
/// level up: not lane-shaped writes mixed with unlaned ones inside a chunk, but a lane-shaped BLOCK
/// extended across a patch shape no lane class covers. The lowering was correct at every count and
/// every flag then as now; what was net-negative was the ADMISSION, and only because of what the
/// killed blocks fell back onto.
///
/// **WHY IT IS ON (2026-08-19/20). THE REFUTATION INVERTED, and the reason is that the fallback got
/// cheap.** `.bench/results/duke-l1-heatgate-20260819/README.md` plus `long/ladder-summary.json`,
/// same binary at main `4bf7a4c8`, all arms selected through this knob, pinned CPU 8, quiet host,
/// DUKEMARK stop `test_exit:81` on every leg and per-arm counters deterministic across legs:
///
/// * duke3d-586-**short**: off floor min 147.27 s over EIGHT legs (spread 2.7%); `on` min 139.05 s
///   over three interleaved legs. **-5.6%.** For scale, the two arms that lost on the same ladder:
///   `heat_gated` +1.6% and `imm8` lanes -1.0%.
/// * duke3d-586 **long merge-gate row**: off 336.82 / 343.80 s, on 315.60 / 315.79 s. **-6.3%
///   min-wall**, rt 0.41 -> 0.44, insns/entry 11.18 -> 14.02, static-unbound -335 M (-23%),
///   coverage -0.7 pp.
///
/// The 08-09 kill-amplification signature is STILL THERE and is no longer wall-dominant:
/// `smc_lane_accepts` 109.0 M -> 91.8 M on the long row (-16%), while `smc_heat_demotions` actually
/// FELL, 147,518 -> 144,136. What changed between the two measurements is the decline path those
/// killed blocks land on -- the sticky-decline memo, the chain-used link mask, warm-fetch-inline
/// and the one-lookup pair all repriced it -- so the same number of kills now costs a fraction of
/// what it cost in August's first week. The short row's census says the same thing from the other
/// side: `rejected` static-unbound target 231.8 M -> 89.6 M (-142 M), entries -20%, insns/entry
/// 11.08 -> 13.69. **The lesson to carry, not just the number: a refutation whose mechanism is
/// "the fallback is expensive" expires when the fallback is optimised, and it does not announce
/// that it has expired.**
///
/// **RE-TEST TRIGGER: a one-byte mutable-imm lane class covering the `imm_len=1` patch shapes
/// (`0xC1`, `0xC0`, `0x80`).** `IMM_LANE_WIDTH` is four and `imm_lane_for`'s accept rule was
/// written against that width, so a 1-byte lane is its own width class rather than a widened
/// match. Once duke's count-byte patches become `lane_accepts` instead of narrow kills, this A/B is
/// measuring something different and must be run again.
///
/// **STATUS 2026-08-19: the width class EXISTS and its first user shipped.** `IMM8_LANE_WIDTH` is
/// one, every lane carries its own width, and `imm8_lane_for` admits the `0x80 /r` third of the
/// trigger behind `IZARRAVM_IMM8_LANES` (default off). `0xC1`/`0xC0` and `0x0FA4` are NOT admitted
/// and are still blocked on THE DESIGN COST below, which is untouched by that slice — the plumbing
/// is no longer the obstacle, the flag-capture split is.
///
/// **STATUS 2026-08-20: THE TRIGGER HAS FIRED for `0xC1`/`0xC0`, AND ITS SLICE IS NOW THE SHIPPED
/// DEFAULT, so EVERY NUMBER ABOVE IS HISTORICAL ON DEFAULTS.** `count_lane_for` admits the group-2
/// COUNT byte as a second `IMM8_LANE_WIDTH` class behind `IZARRAVM_COUNT_LANES`, and THE DESIGN
/// COST below is paid rather than avoided: `emit_rotate_reg_lane` and `emit_shift_lane` carry the
/// compile-time three-way split as a runtime three-way branch over the masked loaded byte. That
/// knob flipped to default ON on the 2026-08-20 ladder (-5.73% short, -4.94% long;
/// `.bench/results/duke-l2-count-lane-20260820/`).
///
/// What that means for the legs recorded above, stated plainly because it is easy to miss: the
/// -5.6% / -6.3% deltas were measured in a world where every count-byte patch KILLED a block, and
/// `smc_lane_accepts` 109.0 M -> 91.8 M was the cost side of that trade. On today's defaults those
/// same patches are absorbed (91.8 M -> 113.4 M) and retire nothing, so **the cost this A/B was
/// trading against no longer exists at the size it had.** An `on`-vs-`off` rotate-rows leg run on
/// current defaults is NOT comparable with the legs above; it is a different measurement of a
/// differently-priced admission, and it should be re-run rather than compared. The four-leg 2x2
/// caution below applies to this pair too. `0x0FA4` SHLD remains the one unlaned third of the
/// trigger.
///
/// **THE 2x2 HAS A CROSS TERM, and a combined ladder leg must be read knowing it.** With
/// `IZARRAVM_IMM8_LANES=1`, a `0x80` patch that a lane absorbs no longer stamps SMC heat on its
/// 16-byte chunk (`lane_only` at core.rs suppresses the bump). The `heat_gated` arm below admits a
/// `0xC1 /0` site exactly when its count byte carries NO heat record — and heat records are
/// per-chunk, so a `0xC1` whose chunk was being kept hot by a neighbouring `0x80` patch becomes
/// admissible once that `0x80` is laned. **So `ROTATE_ROWS=heat_gated` with `IMM8_LANES=1` admits
/// a SUPERSET of the sites it admits with `IMM8_LANES=0`, and the combined leg's delta is not the
/// sum of the two single-lever deltas.** Measure the four legs if the interaction matters; do not
/// subtract one arm's number from the combined one and call the remainder the other arm's.
///
/// THE DESIGN COST THE NEXT SLICE MUST BUDGET FOR, because it is not a lane-plumbing detail. A
/// laned count is loaded at RUNTIME, and `emit_rotate_reg`'s whole correctness argument is a
/// COMPILE-TIME split on the count: 0 emits nothing, 1 captures `CF|OF` and publishes the shadow,
/// 2..31 captures CF alone and goes through `emit_set_cf_only`. A runtime count cannot pick a
/// capture mask at emission, so the lane form is forced onto the CL-shaped emission whose flag
/// update is runtime-conditional -- and the count-0 case is not "some flags" but "no flag moves
/// and no descriptor is created or destroyed", which a conservative publish gets WRONG rather than
/// approximately right. So the lane-form rotate needs either a genuinely conditional runtime flag
/// path (the three-way branch `emit_shift_cl` already declined) or a guard that admits the lane
/// only when the patched count byte's value range excludes 0 and 1. Price that before pricing the
/// lane.
///
/// THE ALTERNATIVE PRICED FIRST, because it might not have needed the lane work at all: **BUILT on
/// 2026-08-19 as the `heat_gated` arm below and MEASURED THE SAME NIGHT, where it LOST.** It admits
/// `0xC1 /0` only at sites whose count byte has no heat record -- the disp-lane heat gate INVERTED,
/// admitting never-patched sites instead of hot ones -- on the theory that duke's ~1.9M count-byte
/// patch events sit on a small number of sites while the 260M runtime hits do not. The unpatched
/// share `u` came out at only **11.4%** of the row's runtime hits (ROL `runtime_hits` 170,902,770 ->
/// 151,490,740, chunk-granular so a lower bound), and the arm cost **+1.6%** wall against the eight-
/// leg off floor. The row's mass sits on HEATED chunks, so gating by heat gives up almost all of the
/// prize to protect against a cost that -- as the `on` arm then showed -- is no longer the dominant
/// term anyway. `heat_gated` stays as a measured arm, not as a candidate default.
///
/// **The knob covers THIS SLICE ONLY.** `0xC1 /1` and `0xD1 /1` ROR at Dword were lowered by the
/// 2026-07-26 slice and are deliberately outside it; so are `/4..=7`. Sweeping them in would make
/// every future A/B price two slices as one. The heat gate is scoped to the same two rows.
///
/// **THREE ARMS SINCE THE 2026-08-19 L1 SLICE**, all shipping in the one executable on the
/// `IZARRAVM_LANE_FAMILY` / `IZARRAVM_DISP_LANES` contract -- this box has measured 6% wall
/// variance between builds of identical source, which is larger than the effect, so a cross-build
/// comparison would not be evidence (`dev_docs/duke-reprofile-2026-08-19.md` §6.2):
///
/// * `IZARRAVM_ROTATE_ROWS` **unset**, or `1` / `on` -> `On`: unconditional admission of both rows.
///   **THE SHIPPED DEFAULT since 2026-08-19/20**, on -5.6% short / -6.3% long. `1` is pinned to
///   this arm rather than merely accepted by it, because `1` is the spelling every historical A/B
///   leg used and keeping it stable is what makes those legs comparable with a leg run on this
///   binary -- including the 2026-08-09 legs it has now contradicted.
/// * `0` or `off` (or an empty value) -> `Off`: the pre-slice refusal. **THE ESCAPE, and THE BASE
///   every A/B on these rows is read against**, because it is byte-for-byte the pre-slice world.
/// * `heat` or `heat_gated` -> `HeatGated`: admit ONLY where the count byte carries no SMC heat
///   record. The L1 slice, measured and beaten by both other arms (+1.6%).
/// * **anything else PANICS.** See `parse_rotate_rows_arm` for why guessing is worse than failing.
///
/// **WHERE EACH ARM IS READ.** `Off` is read at the CLASSIFY admission point, so it reproduces the
/// pre-slice refusal exactly: `classify` returns None, the compile walk breaks, and the row lands
/// back in the census as an ordinary `hard_boundary` unbound exit rather than as some new refusal
/// kind that would not be comparable with the census this slice was ranked against. That is what
/// makes the `off` arm a true pre-slice world and not merely a quiet one. `HeatGated`
/// classifies as `On` does and is narrowed one step later, in the compile walk, where the physical
/// address and the heat map are in scope; it downgrades to the SAME `HardBoundary`, so its
/// refusals are the same census row as the off arm's. `census_native_suffix` mirrors that
/// downgrade -- see the divergence ledger there, which is the only other place `classify` is
/// consulted as if it were the admission.
pub(crate) fn rotate_rows_enabled() -> bool {
    rotate_rows_arm() != RotateRowsArm::Off
}

/// The three-way selection behind `rotate_rows_enabled`. See its doc comment for the contract.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RotateRowsArm {
    /// The pre-slice refusal: `classify` returns None for both rows. Shipped default until
    /// 2026-08-19; now the escape and the A/B base.
    Off,
    /// L1: admit the rows only at sites whose count byte carries no SMC heat record.
    HeatGated,
    /// Unconditional admission. **Today's shipped default**, on the 2026-08-19/20 re-measurement.
    On,
}

pub(crate) fn rotate_rows_arm() -> RotateRowsArm {
    #[cfg(test)]
    if let Some(forced) = ROTATE_ROWS_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ARM: std::sync::OnceLock<RotateRowsArm> = std::sync::OnceLock::new();
    *ARM.get_or_init(|| parse_rotate_rows_arm(std::env::var("IZARRAVM_ROTATE_ROWS")))
}

/// The `IZARRAVM_ROTATE_ROWS` spelling table, lifted out of the `OnceLock` closure so it can be
/// unit-tested without a process-global env write. See `rotate_rows_enabled` for the contract.
///
/// **AN UNRECOGNISED VALUE PANICS, and that is the load-bearing choice here.** The obvious
/// alternative -- fall through to `On` the way the pre-L1 `value != "0"` reading did -- silently
/// turns a mistyped ladder leg (`IZARRAVM_ROTATE_ROWS=heat-gated`, `=heatgated`, `=gated`) into
/// whatever arm the fallthrough names, and the reading of the leg is then a statement about an arm
/// nobody selected. That was worth failing loudly for when the fallthrough was the negative
/// control, and it is worth it MORE now that `On` is the shipped default: a typo would run the
/// default and be read as "the arm I asked for changed nothing", which is the one wrong conclusion
/// an arm ladder exists to avoid. A typo fails at the first compile rather than producing a
/// plausible number.
///
/// Trimmed and case-folded on the way in, because a knob set from a shell script picks up
/// whitespace and a knob set from a PowerShell ladder picks up capitalisation; neither is a
/// different arm and neither should reach the panic.
fn parse_rotate_rows_arm(value: Result<String, std::env::VarError>) -> RotateRowsArm {
    let raw = match value {
        // Unset is the shipped default and the overwhelmingly common case. Since the 2026-08-19/20
        // re-measurement that default is `On`: see `rotate_rows_enabled` for both A/Bs. `0` / `off`
        // is the escape back to the pre-slice refusal.
        Err(std::env::VarError::NotPresent) => return RotateRowsArm::On,
        // Not-UTF-8 is not a spelling of any arm. It reaches the same panic as a typo rather than
        // the same silence as "unset": someone set the variable and meant something by it.
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_ROTATE_ROWS is set to a value that is not valid UTF-8; accepted \
                 spellings are unset or `1` / `on` (the shipped default, unconditional \
                 admission), `0` / `off` (the pre-slice refusal), or `heat` / `heat_gated` (the \
                 L1 heat-gated arm)"
            )
        }
        Ok(raw) => raw,
    };
    // Each arm answers to its DESIGN NAME (`off` / `heat_gated` / `on`, the spellings
    // dev_docs/duke-reprofile-2026-08-19.md §6.2 uses to describe the ladder) as well as to its
    // numeric one. A ladder leg written from the design doc and a leg written from the shell
    // history must reach the same arm, or the accepted-spelling set is itself a trap.
    match raw.trim().to_ascii_lowercase().as_str() {
        // The escape from the shipped default back to the pre-slice refusal. Empty stays here with
        // `0` and `off` rather than following "unset" to `On`: a ladder or wrapper script that
        // computes the value and produces "" meant something falsy, and `IZARRAVM_ROTATE_ROWS=`
        // is the shell's shortest way to say off.
        "" | "0" | "off" => RotateRowsArm::Off,
        "heat" | "heat_gated" => RotateRowsArm::HeatGated,
        // `1` is pinned here rather than merely accepted: the pre-L1 reading was "any value but
        // 0", `1` is the spelling every historical A/B leg used, and keeping it on this arm is
        // what makes those legs comparable with a leg run on this binary. It is now also the
        // explicit spelling of the shipped default, so a ladder that names it stays honest.
        "1" | "on" => RotateRowsArm::On,
        other => panic!(
            "IZARRAVM_ROTATE_ROWS={other:?} names no arm; accepted spellings are unset or `1` / \
             `on` (the shipped default, the 2026-08-19/20 unconditional admission), `0` / `off` \
             (the pre-slice refusal, the escape), or `heat` / `heat_gated` (the L1 heat-gated \
             arm). Refusing to guess: a mistyped ladder leg that silently ran a different arm \
             would be read as the slice under test doing nothing"
        ),
    }
}

// Per-THREAD, because the shipped knob is a process-wide `OnceLock` and the fixtures have to run
// both arms in one process. Thread-local rather than a global is what keeps the parallel test
// harness honest: one test's arm selection cannot reach another's compile.
//
// Not a convenience, in EITHER direction, and the direction that matters flipped on 2026-08-19.
// While the default was OFF, every positive fixture for these two rows had to force the on arm
// through here or it would test the refusal and call it a lowering. Now that the default is ON, it
// is the REFUSAL fixtures that must force `Off` explicitly -- a fixture that means to pin the
// pre-slice boundary and reads the ambient arm would silently compile the rows and pass for the
// wrong reason. Both kinds state their arm; neither leans on the default.
#[cfg(test)]
thread_local! {
    static ROTATE_ROWS_OVERRIDE: std::cell::Cell<Option<RotateRowsArm>> =
        const { std::cell::Cell::new(None) };
}

/// Force the group-2 admission arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_ROTATE_ROWS` reading. The `bool` spelling is the pre-L1 one and keeps naming
/// the two arms it always named; `set_rotate_rows_arm_for_test` reaches the third.
#[cfg(test)]
pub(crate) fn set_rotate_rows_for_test(forced: Option<bool>) {
    set_rotate_rows_arm_for_test(forced.map(|on| {
        if on {
            RotateRowsArm::On
        } else {
            RotateRowsArm::Off
        }
    }));
}

#[cfg(test)]
pub(crate) fn set_rotate_rows_arm_for_test(forced: Option<RotateRowsArm>) {
    ROTATE_ROWS_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. `rotate_rows_arm` caches its env reading in a
/// process-wide `OnceLock`, so the contract is otherwise assertable exactly once per process and
/// never in an order the harness controls.
#[cfg(test)]
pub(crate) fn parse_rotate_rows_arm_for_test(
    value: Result<String, std::env::VarError>,
) -> RotateRowsArm {
    parse_rotate_rows_arm(value)
}

/// Seed for `JitState::word_at_486`, read once per process from `IZARRAVM_JIT16_486`.
///
/// **DEFAULT ON since the 486 measurement.** Set `IZARRAVM_JIT16_486=0` to refuse.
///
/// Separate from `IZARRAVM_JIT16` on purpose: that one selects WHICH memory a 16-bit code segment
/// may live in, and this one selects WHICH PERSONAS lower Word operands at all. They compose, and
/// keeping them independent is what let the two halves be measured apart — an
/// `IZARRAVM_JIT16=0` arm isolates this flag's 32-bit half (66-prefixed word ops) exactly, because
/// `try_direct_continuation` then refuses every 16-bit boundary before a key is built.
///
/// The design that introduced this said to DELETE the knob when the default flipped, so a
/// temporary switch could not become permanent surface. It stays, deliberately, for two reasons
/// the design did not know yet: the flip ships a measured ~4% regression on quake-586, so an
/// escape hatch is worth its surface until coverage work closes that; and the differential tests
/// that cover the refusing arm at I486 have no other way to reach it once the lift is
/// unconditional. Delete it when quake-586 is back at parity, not before.
pub(crate) fn word_at_486_default() -> bool {
    static LEVEL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| !matches!(std::env::var("IZARRAVM_JIT16_486").as_deref(), Ok("0")))
}

/// Whether a `Dormant` key parked for a CLEARABLE compile-walk cause is ever re-probed
/// (`BlockCache::lift_clearable_retry_dormant`).
///
/// **DEFAULT OFF.** `1` or `on` admits the lift; unset, `0` or `off` keep every compile-Retry park
/// permanent, which is the behaviour every measurement before 2026-08-22 was taken under.
///
/// WHY IT SHIPS DISABLED. `duke3d-586-short` regressed 109.9 -> 121.0 s between the S4 part-1
/// binary and part 2, and the lift is the only part-2 change that alters WHICH keys get compiled,
/// so it is the first thing an A/B has to be able to remove. It is NOT established as the cause,
/// and the arithmetic argues against it: 1,961 lifts on that run, against duke's measured ~16 us
/// per compile, is about 31 ms of compile time inside an 11 s regression. The counter signature
/// there -- `linked_transfers` -99 M, `jit_direct_insns` -264 M, entries +54 M -- is a chaining
/// and block-shape collapse, which is a different family of cause. Defaulting off is what makes
/// the next duke run able to say so instead of guessing.
///
/// It also has no measurement in its favour yet. `retry_lifts` / `retry_lift_reparks` read 1,961
/// and 10,265 on that run: more keys came back than were ever lifted. That is not by itself an
/// indictment (see the counter's own doc for why the second number counts park EVENTS), but an
/// arm whose only evidence is a ratio pointing the wrong way does not belong on by default.
pub(crate) fn retry_lift_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = RETRY_LIFT_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| parse_retry_lift_arm(std::env::var("IZARRAVM_RETRY_LIFT")))
}

/// The `IZARRAVM_RETRY_LIFT` spelling table. See `retry_lift_enabled`.
fn parse_retry_lift_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset = OFF, so the PowerShell nulling trap damages the ON leg: an ON leg must EXPORT
        // `1`, and a leg that merely nulls the variable measures the default.
        Err(std::env::VarError::NotPresent) => return false,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_RETRY_LIFT is set to a value that is not valid UTF-8; accepted \
                 spellings are unset, `0` or `off` (the shipped default: a compile-Retry park is \
                 permanent), and `1` or `on` (re-probe a key whose cause is clearable after \
                 RETRY_LIFT_VISITS visits)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_RETRY_LIFT={other:?} names no arm; accepted spellings are unset, `0` or \
             `off` (the shipped default) and `1` or `on` (the retry lift). \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be read \
             as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `TEST_WORD_ROWS_OVERRIDE`'s reason.
#[cfg(test)]
thread_local! {
    static RETRY_LIFT_OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Force the retry-lift arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_RETRY_LIFT` reading.
#[cfg(test)]
pub(crate) fn set_retry_lift_for_test(forced: Option<bool>) {
    RETRY_LIFT_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. See `parse_test_word_rows_arm_for_test`.
#[cfg(test)]
pub(crate) fn parse_retry_lift_arm_for_test(value: Result<String, std::env::VarError>) -> bool {
    parse_retry_lift_arm(value)
}

/// Whether a segment-loading `InterpretOne` call-out may RESUME its block when the record it moved
/// is one no other slot in the block uses (design section 11, S4f).
///
/// **DEFAULT OFF SINCE 2026-08-22: REFUTED ON THE LOADER.** Unset, `0` or `off` all keep the
/// pre-S4f behaviour, which is what ships: R2 compares all six records for these rows, so any
/// change resyncs, and the block publishes its successors because nothing feeds
/// `callout_segment_writes`. `1` or `on` admits the relaxation.
///
/// THE MEASUREMENT, in full, because a refuted slice that ships whole is only worth its evidence.
/// Loader phase, ONE binary (`9138d554`), knob OFF against knob ON, A/A 1.0117 and every pin
/// identical: **ON is 10.5% SLOWER** (median 0.8945, lower95 0.8903). Entries 11,759,636 off
/// against 12,918,338 on; native instructions 106.3 M against 108.2 M.
///
/// WHY, and it is not the reason the first gate suggested. That gate ran the ON arm alone against
/// the S3 head, read -9%, and named two counters moving the wrong way:
/// `jit_direct_reject_data_segment` 307,714 -> 514,327 with compile attempts up by the same
/// 206,000, and `segment_write_block_head_entries` at 1,959,263. The first was a real defect and
/// is fixed -- the mask is the whole block now, so a resumed slot never moves a record its own
/// block bakes -- and fixing it did not rescue the slice. What dominates is the second, which is
/// not a defect at all: a block holding a segment-writing call-out publishes NO successors, so the
/// instructions absorbed behind the slot lose their own outbound chaining and every one of them
/// pays a dispatcher round trip that a boundary at the load would not have cost. Design review
/// 11.1 M3 named that cost and expected the loader's links not to bind often enough for it to
/// matter; on this fixture it outweighs the round trips the relaxation saves, and the entry count
/// says so directly: admitting the rows ADDS 1.16 M entries.
///
/// WHAT WOULD CHANGE THE ANSWER, recorded so the refutation does not have to be rediscovered. The
/// successor bar is what costs, and it is there because a chained transfer skips `data_matches`.
/// A block that could re-run the entry check at a chained transfer, or a mask discipline the LINK
/// could carry the way `merge_chain` carries the chain-used one, would let the tails keep their
/// chaining and leave only the win. That is a link-side slice, not a call-out one.
///
/// The whole ON arm still ships -- its mask, its cells, its counter and its fixtures -- because it
/// is the base every future A/B on this question is read against.
///
/// Read ONCE PER COMPILE and baked into the slot's cell, not read at run time. A block therefore
/// keeps the arm it was compiled under for its whole life, which is what makes an interleaved A/B
/// mean anything: flipping the knob under a live cache would leave blocks from both arms running
/// and the counters unattributable.
pub(crate) fn callout_segment_resume_enabled() -> bool {
    #[cfg(test)]
    if let Some(forced) = CALLOUT_SEGMENT_RESUME_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        parse_callout_segment_resume_arm(std::env::var("IZARRAVM_CALLOUT_SEGMENT_RESUME"))
    })
}

/// The `IZARRAVM_CALLOUT_SEGMENT_RESUME` spelling table. See `callout_segment_resume_enabled`.
fn parse_callout_segment_resume_arm(value: Result<String, std::env::VarError>) -> bool {
    let raw = match value {
        // Unset = OFF since the 2026-08-22 refutation, which INVERTS the empty-string trap rather
        // than removing it. The trap itself is unchanged: NULLING the variable is not unsetting it
        // -- PowerShell leaves it present and empty, and the empty string is spelled OFF two arms
        // down. What moved is which leg it damages. Under a default-ON knob a nulled variable
        // silently ran the OFF arm; under this one it silently runs the DEFAULT, so an ON leg is
        // the one that must EXPORT `1` and a leg that merely nulls the variable measures nothing.
        Err(std::env::VarError::NotPresent) => return false,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_CALLOUT_SEGMENT_RESUME is set to a value that is not valid UTF-8; \
                 accepted spellings are unset, `0` or `off` (the shipped default since the \
                 2026-08-22 refutation: R2 compares all six records for a segment-loading call-out \
                 and the block keeps its successors), and `1` or `on` (the S4f relaxation, which \
                 the loader measured at 10.5% SLOWER)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => false,
        "1" | "on" => true,
        other => panic!(
            "IZARRAVM_CALLOUT_SEGMENT_RESUME={other:?} names no arm; accepted spellings are unset, \
             `0` or `off` (the shipped default: any changed record resyncs and the block publishes \
             its successors), and `1` or `on` (the S4f relaxation: the whole-block segment mask, \
             and the successor bar that pays for it). \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be read \
             as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `TEST_WORD_ROWS_OVERRIDE`'s reason: the shipped knob is a process-wide
// `OnceLock` and the fixtures have to run both arms in one process.
#[cfg(test)]
thread_local! {
    static CALLOUT_SEGMENT_RESUME_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}

/// Force the segment-resume arm on this thread for the length of a fixture; `None` restores the
/// ambient `IZARRAVM_CALLOUT_SEGMENT_RESUME` reading.
#[cfg(test)]
pub(crate) fn set_callout_segment_resume_for_test(forced: Option<bool>) {
    CALLOUT_SEGMENT_RESUME_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. See `parse_test_word_rows_arm_for_test`.
#[cfg(test)]
pub(crate) fn parse_callout_segment_resume_arm_for_test(
    value: Result<String, std::env::VarError>,
) -> bool {
    parse_callout_segment_resume_arm(value)
}

/// Whether the Direct backend lowers `OperandSize::Word` operands on this CPU.
///
/// ONE predicate for what used to be three copies of `persona != I586`: the compile walk's Word
/// refusal, `key_for_phys`'s 16-bit-segment refusal, and the census suffix scan's copy of the
/// first. They have to move together. The compile walk and `key_for_phys` are COUPLED by
/// construction — `key_for_phys` refuses the key precisely BECAUSE the walk would reject the first
/// slot and install a rejected span for zero yield — so lifting either alone is wrong in a
/// different direction: the walk alone is inert, the key alone is pure churn. The census copy is
/// the one that would silently re-open a seventh divergence between the two walks.
///
/// The 16-bit half rests on an identity worth stating, because it is what lets one predicate serve
/// both questions: `operand_size` follows CS.D opcode-independently, so in a CS.D = 0 segment
/// EVERY instruction decodes at `Word`. "May a 16-bit segment be keyed" and "are Word operands
/// admitted" are therefore the same question asked at two points.
///
/// The admitted set is I486 and I586, never the 386 class. Interpreted 386 already runs above 15x
/// real time, so there is no throughput problem for the JIT to solve there, and admitting it would
/// widen the blast radius of every Word lowering to a persona nobody benchmarks. `key_for_phys`
/// already refuses every persona below I486 a few lines up, so the 386 class is doubly excluded;
/// spelling it out here means a future 386 enablement cannot silently inherit Word admission.
///
/// Be honest about that arm: it is UNREACHABLE today and therefore UNTESTABLE. Flipping
/// `I386 => false` to `true` fails nothing, because `key_for_phys`'s own persona check runs first
/// and the compile walk is reached only through it. It is defence in depth against a future edit
/// to that check, not a live guard, and no test should be written that pretends otherwise.
pub(crate) fn word_operands_admitted(cpu: &CpuGsw) -> bool {
    match cpu.persona() {
        CpuPersona::I586 => true,
        CpuPersona::I486 => cpu.jit_direct.word_at_486,
        CpuPersona::I386 => false,
    }
}

/// Which arm of the data-segment reject governor this process runs.
///
/// **DEFAULT `cap` SINCE THE 2026-08-23 LADDER.** The spelling table, and note that unset and the
/// EMPTY STRING are deliberately NOT the same arm:
///
/// | spelling | arm |
/// |---|---|
/// | **unset** | **`cap`** — the shipped default: the per-key retire cap (`DATA_SEGMENT_RETIRE_CAP`) |
/// | `""`, `0`, `off` | `Off` — pre-slice behaviour, every data-segment reject retires its key |
/// | `cap` | `Cap`, stated explicitly; identical to unset |
/// | `1`, `on` | `On` — the cap PLUS the stage 2 link decline |
/// | anything else | panics |
///
/// WHY IT SHIPS ARMED. The pre-registered ladder on `2d126c5e`, one binary, ABBA interleaved:
/// **tombraid loader phase `cap` +27.1% (lower95 1.263), `on` +28.8%; duke3d-586 short `cap`
/// +4.3%, `on` +3.5%**, every pin identical on every leg. The mechanism is not subtle -- the
/// loader was paying 0.94 s of a 7.09 s phase to recompile blocks it then refused to run, 97.9%
/// of its compile attempts, and the cap takes `jit_direct_compile_attempts` from 314,318 to 9,695.
///
/// WHY `on` IS **NOT** THE DEFAULT even though it wins the loader by 1.7 points more. It LOSES to
/// `cap` on duke (+3.5% against +4.3%), which is the shape the design predicted for it (a declined
/// key trades chaining for dispatcher entries) and is the refutation criterion §5 wrote down for
/// stage 2 in advance. Two fixtures disagreeing is a reason to ship the arm that never loses and
/// keep the other behind the knob, not a reason to average them. `on` stays OPT-IN.
///
/// WHAT `cap` COSTS, stated because it is not free. Corpus, OFF against `cap`: doom +0.6%,
/// quake -1.3%, wolf3d -1.2%, tombraid +2.7%. The two dips are the residual §3(c) named: a key
/// that is frozen on one layout INTERPRETS every entry that carries another, forever, where the
/// pre-slice retire would have re-specialized it and run. On a guest whose record settles after a
/// few moves the retire was the right bet and the cap now refuses to take it. The answer is not a
/// bigger cap -- it is slice (a), per-layout block VARIANTS, whose go/no-go census this slice
/// ships and whose `cap`-leg reading is in the design's §R rev 5.
///
/// THE ESCAPE, AND THE A/B BASE, IS NOW `off`. It touches no map, reads no set, and does not even
/// build the governor's two inputs (`run_direct_block` tests this knob before it builds them), so
/// an OFF leg is a reproduction of pre-slice `main` at this site rather than a close relative of
/// one. `the_off_arm_touches_no_governor_state` pins that.
///
/// **THE NULLING TRAP IS INVERTED HERE, AND IT MATTERS.** Everywhere else in this file unset means
/// OFF, so `env-null-empty-is-off-trap` bites the ON leg. Here unset means `cap`, and `""` means
/// OFF -- so PowerShell's `SetEnvironmentVariable($null)`, which leaves `""` behind, silently
/// DISARMS the shipped default. A leg that means to measure the default must leave the variable
/// genuinely unset or EXPORT `cap`; a leg that nulls it is measuring the escape.
pub(crate) fn segment_retire_governor() -> super::SegmentRetireGovernor {
    #[cfg(test)]
    if let Some(forced) = SEGMENT_RETIRE_GOVERNOR_OVERRIDE.with(std::cell::Cell::get) {
        return forced;
    }
    static ARM: std::sync::OnceLock<super::SegmentRetireGovernor> = std::sync::OnceLock::new();
    *ARM.get_or_init(|| {
        parse_segment_retire_governor_arm(std::env::var("IZARRAVM_SEGMENT_RETIRE_GOVERNOR"))
    })
}

/// The `IZARRAVM_SEGMENT_RETIRE_GOVERNOR` spelling table. See `segment_retire_governor`.
fn parse_segment_retire_governor_arm(
    value: Result<String, std::env::VarError>,
) -> super::SegmentRetireGovernor {
    use super::SegmentRetireGovernor;
    let raw = match value {
        // UNSET IS `cap`, and `""` below is OFF. The two are deliberately different arms: see the
        // inverted nulling trap in `segment_retire_governor`'s doc.
        Err(std::env::VarError::NotPresent) => return SegmentRetireGovernor::Cap,
        Err(std::env::VarError::NotUnicode(_)) => {
            panic!(
                "IZARRAVM_SEGMENT_RETIRE_GOVERNOR is set to a value that is not valid UTF-8; \
                 accepted spellings are unset or `cap` (the shipped default: the per-key retire \
                 cap), `\"\"`, `0` or `off` (the escape: every data-segment reject retires, as \
                 before the slice) and `1` or `on` (the cap plus the link decline)"
            )
        }
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => SegmentRetireGovernor::Off,
        "cap" => SegmentRetireGovernor::Cap,
        "1" | "on" => SegmentRetireGovernor::On,
        other => panic!(
            "IZARRAVM_SEGMENT_RETIRE_GOVERNOR={other:?} names no arm; accepted spellings are \
             unset or `cap` (the shipped default, stage 1's per-key retire cap), `\"\"`, `0` or \
             `off` (the escape and the A/B base) and `1` or `on` (stage 1 plus the link decline). \
             Refusing to guess: a mistyped leg would silently run the DEFAULT, which is now an \
             ARMED arm, and be read as the arm it named doing nothing"
        ),
    }
}

// Per-THREAD, for `TEST_WORD_ROWS_OVERRIDE`'s reason.
#[cfg(test)]
thread_local! {
    static SEGMENT_RETIRE_GOVERNOR_OVERRIDE: std::cell::Cell<Option<super::SegmentRetireGovernor>> =
        const { std::cell::Cell::new(None) };
}

/// Force the governor arm on this thread for the length of a fixture; `None` restores the ambient
/// `IZARRAVM_SEGMENT_RETIRE_GOVERNOR` reading.
#[cfg(test)]
pub(crate) fn set_segment_retire_governor_for_test(forced: Option<super::SegmentRetireGovernor>) {
    SEGMENT_RETIRE_GOVERNOR_OVERRIDE.with(|cell| cell.set(forced));
}

/// The spelling table, reachable from the fixtures. See `parse_retry_lift_arm_for_test`.
#[cfg(test)]
pub(crate) fn parse_segment_retire_governor_arm_for_test(
    value: Result<String, std::env::VarError>,
) -> super::SegmentRetireGovernor {
    parse_segment_retire_governor_arm(value)
}
/// Arm for the entry-attribution observer (`IZARRAVM_DIRECT_ENTRY_ATTRIBUTION`).
///
/// The instrument itself is compiled out entirely without the `direct-entry-attribution` feature,
/// so the plain build carries no such symbol at all; this gate exists only inside the observer
/// build, and its default is still OFF.
///
/// * **unset**, `` (empty), `0` or `off` -> OFF. The default, and the disarmed leg A4 compares
///   against on the SAME observer binary. **The empty string is OFF and so is unset**, unlike the
///   lane family where unset is the shipped ON default: nulling a variable in PowerShell leaves it
///   PRESENT and EMPTY, and this knob's default arm is the off arm, so both spellings agree.
/// * `1` / `on` / `full` -> FULL. All sixteen phases, >= 16 marks per entry.
/// * `2` / `coarse` -> COARSE. Four marks — `begin`, the native window in, the native window out,
///   `end` — whose totals must agree with FULL's to within 5% (A6).
/// * **anything else PANICS**, for `parse_disp_lanes_arm`'s reason: a mistyped ladder leg would
///   silently run the DEFAULT and be read as the arm it named doing nothing.
#[cfg(feature = "direct-entry-attribution")]
pub(crate) fn entry_attribution_arm() -> crate::jit::direct::entry_attribution::Arm {
    static ARM: std::sync::OnceLock<crate::jit::direct::entry_attribution::Arm> =
        std::sync::OnceLock::new();
    *ARM.get_or_init(|| {
        parse_entry_attribution_arm(std::env::var("IZARRAVM_DIRECT_ENTRY_ATTRIBUTION"))
    })
}

/// The `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION` spelling table, lifted out of the `OnceLock` closure so
/// it can be unit-tested without a process-global env write. See `entry_attribution_arm`.
#[cfg(feature = "direct-entry-attribution")]
pub(crate) fn parse_entry_attribution_arm(
    value: Result<String, std::env::VarError>,
) -> crate::jit::direct::entry_attribution::Arm {
    use crate::jit::direct::entry_attribution::Arm;
    const ACCEPTED: &str = "accepted spellings are unset or `` / `0` / `off` (the default, the \
                            instrument disarmed), `1` / `on` / `full` (all sixteen phases), and \
                            `2` / `coarse` (four marks)";
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return Arm::Off,
        Err(std::env::VarError::NotUnicode(_)) => panic!(
            "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION is set to a value that is not valid UTF-8; \
             {ACCEPTED}"
        ),
        Ok(raw) => raw,
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "off" => Arm::Off,
        "1" | "on" | "full" => Arm::Full,
        "2" | "coarse" => Arm::Coarse,
        other => panic!(
            "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION={other:?} names no arm; {ACCEPTED}. Refusing to \
             guess: a mistyped ladder leg would silently run the DEFAULT and be read as the arm \
             it named doing nothing"
        ),
    }
}

/// Sampling stride for the entry-attribution observer
/// (`IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE`). The decision is taken at `begin()`, so a sampled
/// traversal is stamped end to end and the stride does not inflate any phase.
///
/// * **unset** or `` (empty) -> 1, every traversal.
/// * a positive integer -> that stride.
/// * **anything else, including `0`, PANICS.**
#[cfg(feature = "direct-entry-attribution")]
pub(crate) fn entry_attribution_sample() -> u64 {
    static SAMPLE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *SAMPLE.get_or_init(|| {
        parse_entry_attribution_sample(std::env::var("IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE"))
    })
}

/// The `IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE` spelling table. See `entry_attribution_sample`.
#[cfg(feature = "direct-entry-attribution")]
pub(crate) fn parse_entry_attribution_sample(value: Result<String, std::env::VarError>) -> u64 {
    const ACCEPTED: &str = "accepted spellings are unset or `` (every traversal) and a positive \
                            integer stride";
    let raw = match value {
        Err(std::env::VarError::NotPresent) => return 1,
        Err(std::env::VarError::NotUnicode(_)) => panic!(
            "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE is set to a value that is not valid UTF-8; \
             {ACCEPTED}"
        ),
        Ok(raw) => raw,
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return 1;
    }
    match trimmed.parse::<u64>() {
        Ok(stride) if stride >= 1 => stride,
        _ => panic!(
            "IZARRAVM_DIRECT_ENTRY_ATTRIBUTION_SAMPLE={trimmed:?} names no stride; {ACCEPTED}. \
             Refusing to guess: a mistyped ladder leg would silently run the DEFAULT and be read \
             as the arm it named doing nothing"
        ),
    }
}
