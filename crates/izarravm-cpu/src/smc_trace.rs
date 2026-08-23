// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Off-by-default self-modifying-code trace: what the guest patches, where inside the patched
//! instruction, and which invalidation the write paid for.
//!
//! Diagnostic only. It exists to answer one factual question before any mutable-lane work is
//! built: are the hot SMC sites on a fixture immediate-field patches of an otherwise invariant
//! instruction (Doom's renderer patches the `imm32` of `addl ebp, imm32` before every column and
//! span), or structural rewrites that a parameterized block could never survive?
//!
//! The gate lives at the CALL SITE in `note_code_write_hit`, not inside this module: with the
//! trace off the caller's `Option` is `None`, nothing here is reached, and no argument is built.
//! That is the default-off-instruments lesson - a callee-side `enabled` check still pays for the
//! arguments on every one of the caller's invocations.
//!
//! READING THE OUTPUT. Two properties bound what a report can be used to claim, and both have
//! already produced an overclaim once:
//!
//! - A site row's class, decoded identity, and field offset FREEZE at the site's first hit (the
//!   `or_insert_with` in `record`); later hits only accumulate counters. A row is therefore one
//!   observation plus a hit count, never evidence that every hit at that address had that shape -
//!   the same address can also take hits that classify differently, and the per-event class totals
//!   are the authority whenever the two disagree. **Design consequence for the parameterized-block
//!   work: a mutable lane must classify EVERY write at runtime against the shape currently decoded
//!   at that address. A per-site shape learned from this trace does not license one, and building
//!   on one would be unsound.**
//! - Only `REPORT_SITES` sites are printed, out of a site map that runs to six figures on a real
//!   fixture. Site sums are therefore lower bounds; only the class totals are complete. Never
//!   report a site sum and a class total against each other without saying which is which.
//!
//! Values: this records the immediate the decode cache held for the covering line, which is the
//! value present BEFORE the write being traced. For a site the guest patches repeatedly that is
//! exactly the value its PREVIOUS patch wrote (the write kills the line, the next execution
//! re-decodes the new bytes), so the distinct-value count faithfully counts distinct patched
//! values, offset by one observation. The post-write bytes are not available at the invalidation
//! choke and are deliberately not plumbed through it.

use std::collections::HashMap;

use crate::{DecodedInsn, Prefixes};

/// How the write landed inside the instruction it overwrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SmcFieldClass {
    /// Every written byte fell inside the instruction's immediate field.
    ImmediateOnly,
    /// Every written byte fell inside the displacement field.
    DisplacementOnly,
    /// The write touched an opcode, prefix, or ModRM/SIB byte, straddled a field boundary, or ran
    /// off the end of the instruction.
    Structural,
    /// The write hit a marked code byte but no live decode line covered it (a displaced or
    /// already-invalidated line, or a compiled-block-only hit).
    NoCoveringLine,
}

const CLASS_LABELS: [&str; 4] = ["imm", "disp", "structural", "no-line"];

impl SmcFieldClass {
    fn index(self) -> usize {
        match self {
            Self::ImmediateOnly => 0,
            Self::DisplacementOnly => 1,
            Self::Structural => 2,
            Self::NoCoveringLine => 3,
        }
    }

    fn label(self) -> &'static str {
        CLASS_LABELS[self.index()]
    }
}

/// What the traced write cost, read off the same locals the production invalidation path uses.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SmcTraceAction {
    /// Compiled Direct blocks retired by this write.
    pub(crate) blocks_killed: u32,
    /// Decode lines killed by the narrow path.
    pub(crate) narrow_kills: u32,
    /// The narrow path was not sound for this write, so the whole decode cache and the Direct
    /// translation were flushed.
    pub(crate) wholesale: bool,
    /// 16-byte chunks that crossed the SMC heat threshold on this write. Each one demotes its
    /// chunk to the interpreter for the rest of the heat epoch.
    pub(crate) newly_hot: u32,
}

/// The pre-write facts a traced write needs, gathered before the invalidation mutates anything.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SmcTracePre {
    /// The live decode line covering the write's first byte: its physical start and decoded form.
    covering: Option<(u32, DecodedInsn)>,
    /// Retired-instruction count at the write, for the per-site patch interval.
    instructions: u64,
}

impl SmcTracePre {
    pub(crate) fn new(covering: Option<(u32, DecodedInsn)>, instructions: u64) -> Self {
        Self {
            covering,
            instructions,
        }
    }
}

/// The decoded identity of the overwritten instruction, flattened for reporting.
#[derive(Debug, Clone, Copy)]
struct InsnIdentity {
    start: u32,
    len: u8,
    opcode: u16,
    modrm: Option<(u8, u8, u8)>,
    imm_len: u8,
    disp_len: u8,
    operand_bytes: u32,
    /// Whether the overwritten instruction carried NO prefixes at all. It is a bool rather than
    /// the `Prefixes` value because that is the only distinction any lane matcher makes
    /// (`prefixes == Prefixes::default()` is a bar on all three), and it is what keeps a
    /// `0x66`-prefixed store out of the same `disp_store` cell as the shapes the arm admits.
    default_prefixes: bool,
}

/// One traced write address. Sites are keyed by the write's physical address and width, so a
/// guest that patches the same field at two widths shows both.
#[derive(Debug, Clone)]
struct Site {
    write_physical: u32,
    width: u32,
    hits: u64,
    class: SmcFieldClass,
    identity: Option<InsnIdentity>,
    /// Byte offset of the write's first byte inside the covering instruction.
    field_offset: u32,
    /// Distinct pre-write immediates seen, capped so a runaway site cannot grow without bound.
    values: Vec<u32>,
    values_overflow: bool,
    blocks_killed: u64,
    narrow_kills: u64,
    wholesale: u64,
    newly_hot: u64,
    last_instructions: u64,
    min_interval: u64,
    max_interval: u64,
}

/// Distinct pre-write immediates retained per site. Enough to tell "one constant" from "a fresh
/// value every patch" without letting a pathological site allocate unboundedly.
const MAX_VALUES_PER_SITE: usize = 64;

/// Sites written to the report. The tail is covered by the class totals instead, which is the
/// whole reason the module docs forbid reading a site sum as a complete family count.
const REPORT_SITES: usize = 64;

/// The trace: a per-(address, width) site map plus per-class run totals.
#[derive(Debug, Default)]
pub(crate) struct SmcTrace {
    sites: HashMap<u64, Site>,
    events: u64,
    class_events: [u64; 4],
    class_blocks_killed: [u64; 4],
    class_narrow_kills: [u64; 4],
    class_wholesale: [u64; 4],
    class_newly_hot: [u64; 4],
    /// Per-EVENT histogram of the overwritten instruction's shape, for the imm and disp classes
    /// only. This is the mutable-lane shopping list the site rows cannot provide: a site's class
    /// and identity FREEZE at its first hit (see the module docs), so a fixture whose hot sites
    /// first appear as no-line hides every later imm-field patch identity from the ranked rows.
    /// Keyed per event, this table is complete the way the class totals are. The key packs
    /// (class, opcode, modrm reg or 0xFF, insn len, imm_len, disp_len, operand bytes).
    shape_events: HashMap<u64, u64>,
    /// THE `disp_store` CENSUS ROW, owed by the register-pressure contract's standing rule
    /// against unmeasured admissions and by the 2026-08-23 settling census §6.3.
    ///
    /// One row per (opcode, modrm reg, `disp_len`, prefix state) over the four MOV displacement
    /// opcodes -- `0x88`, `0x89`, `0x8A`, `0x8B` -- inside the `disp` class, carrying `events`,
    /// `blocks_killed`, `narrow_kills` and, decisively, `newly_hot`. Complete per event the way
    /// the class totals are, and joinable to `smc_shape` on `opcode` + `modrm_reg` by summing the
    /// cells that share those two fields.
    ///
    /// `disp_len` and the prefix state are IN THE KEY because the matchers bar on them. A row
    /// keyed on (opcode, modrm reg) alone is a SUPERSET of what any arm can admit -- it sums the
    /// disp8 and `0x66`-prefixed forms into the same cell -- and its `newly_hot` would then
    /// overstate the capture numerator. The `admissible` column reports the two bars per row.
    ///
    /// WHY `newly_hot` PER OPCODE IS THE POINT. Option D's capture fraction has until now been
    /// estimated as joined un-laned-disp crossings over the whole run's `smc_heat_chunks_hot`.
    /// That denominator is inflated by every other family: between the P1 and the settling census
    /// legs it rose 22.05% entirely inside the ALREADY-LANED `0x8A` population, dropping the
    /// measured capture from 15.634% to 12.797% while the numerator stayed flat to 0.1%. With
    /// crossings attributed per opcode the ratio no longer has to be estimated through a
    /// whole-run number a family Option D does not touch can move.
    ///
    /// The other three opcodes ride with the two Option D ones deliberately: `0x8A` is the
    /// SHIPPED lane class, and its kill rate is the control the store arm's admission has to be
    /// compared against (0.0026% of joined block kills against un-laned disp's 1.409%). A row
    /// that could only see the new arms could not make that comparison.
    disp_store_rows: HashMap<u32, DispStoreRow>,
}

/// One `disp_store` census row: a (opcode, modrm reg) cell of the `disp` class.
#[derive(Debug, Clone, Copy, Default)]
struct DispStoreRow {
    events: u64,
    blocks_killed: u64,
    narrow_kills: u64,
    newly_hot: u64,
}

/// Which Option D arm an opcode belongs to, reported beside every `disp_store` row so a reader
/// does not have to carry the mapping. `0x8A` is the shipped control, not an arm.
fn disp_store_arm(opcode: u16) -> Option<&'static str> {
    match opcode {
        0x88 | 0x89 => Some("store"),
        0x8b => Some("load_widen"),
        0x8a => Some("laned_8a"),
        _ => None,
    }
}

impl SmcTrace {
    /// Classify one write against the instruction it overwrote. `physical`/`width` describe the
    /// store; `start`/`insn` describe the live decode line covering its first byte.
    fn classify(physical: u32, width: u32, start: u32, insn: &DecodedInsn) -> SmcFieldClass {
        let len = u32::from(insn.len);
        let offset = physical.wrapping_sub(start);
        // A write leaving the instruction is structural either way: it also damages what follows.
        if offset >= len || width > len - offset {
            return SmcFieldClass::Structural;
        }
        let imm_len = u32::from(insn.imm_len);
        let disp_len = u32::from(insn.disp_len);
        // Encoding order: prefixes, opcode, ModRM/SIB, displacement, immediate. The immediate is
        // last, the displacement immediately before it.
        let imm_start = len - imm_len;
        if imm_len > 0 && offset >= imm_start {
            return SmcFieldClass::ImmediateOnly;
        }
        let disp_start = imm_start - disp_len;
        if disp_len > 0 && offset >= disp_start && offset + width <= imm_start {
            return SmcFieldClass::DisplacementOnly;
        }
        SmcFieldClass::Structural
    }

    /// Record one traced SMC write. Called only from the trace-on branch of the invalidation
    /// choke, after the invalidation has run, so `action` is the production outcome.
    pub(crate) fn record(
        &mut self,
        physical: u32,
        width: u32,
        pre: SmcTracePre,
        action: SmcTraceAction,
    ) {
        let (class, identity, field_offset) = match pre.covering {
            Some((start, insn)) => (
                Self::classify(physical, width, start, &insn),
                Some(InsnIdentity {
                    start,
                    len: insn.len,
                    opcode: insn.opcode,
                    modrm: insn.modrm.map(|m| (m.mode, m.reg, m.rm)),
                    imm_len: insn.imm_len,
                    disp_len: insn.disp_len,
                    operand_bytes: insn.operand_size.bytes(),
                    default_prefixes: insn.prefixes == Prefixes::default(),
                }),
                physical.wrapping_sub(start),
            ),
            None => (SmcFieldClass::NoCoveringLine, None, 0),
        };
        let index = class.index();
        self.events += 1;
        self.class_events[index] += 1;
        if matches!(
            class,
            SmcFieldClass::ImmediateOnly | SmcFieldClass::DisplacementOnly
        ) && let Some(identity) = identity
        {
            let key = (u64::from(index as u8) << 56)
                | (u64::from(identity.opcode) << 40)
                | (u64::from(identity.modrm.map_or(0xFF, |(_, reg, _)| reg)) << 32)
                | (u64::from(identity.len) << 24)
                | (u64::from(identity.imm_len) << 16)
                | (u64::from(identity.disp_len) << 8)
                | u64::from(identity.operand_bytes as u8);
            *self.shape_events.entry(key).or_insert(0) += 1;
        }
        // The `disp_store` row, charged on the same event and from the same `action` as the class
        // totals beside it, so the two can never disagree about one write.
        //
        // THE KEY CARRIES `disp_len` AND THE PREFIX STATE, not just (opcode, modrm reg), because
        // the matchers do: a `0x89 /1` with a disp8, or one behind a `0x66`, is REFUSED by
        // `disp_store_lane_for` and would otherwise be summed into the same cell as the disp32
        // forms the arm actually admits. Keyed on (opcode, modrm reg) alone the row is a superset
        // of the admissible population and its capture numerator reads high. Keyed this way each
        // cell is either entirely admissible or entirely not, and the (opcode, modrm reg) join to
        // `smc_shape` still works by summing the cells that share those two fields.
        if class == SmcFieldClass::DisplacementOnly
            && let Some(identity) = identity
            && disp_store_arm(identity.opcode).is_some()
        {
            let key = (u32::from(identity.opcode) << 16)
                | (u32::from(identity.modrm.map_or(0xFF, |(_, reg, _)| reg)) << 8)
                | (u32::from(identity.disp_len) << 1)
                | u32::from(identity.default_prefixes);
            let row = self.disp_store_rows.entry(key).or_default();
            row.events += 1;
            row.blocks_killed += u64::from(action.blocks_killed);
            row.narrow_kills += u64::from(action.narrow_kills);
            row.newly_hot += u64::from(action.newly_hot);
        }
        self.class_blocks_killed[index] += u64::from(action.blocks_killed);
        self.class_narrow_kills[index] += u64::from(action.narrow_kills);
        self.class_wholesale[index] += u64::from(action.wholesale);
        self.class_newly_hot[index] += u64::from(action.newly_hot);

        let key = (u64::from(physical) << 8) | u64::from(width & 0xff);
        let site = self.sites.entry(key).or_insert_with(|| Site {
            write_physical: physical,
            width,
            hits: 0,
            class,
            identity,
            field_offset,
            values: Vec::new(),
            values_overflow: false,
            blocks_killed: 0,
            narrow_kills: 0,
            wholesale: 0,
            newly_hot: 0,
            last_instructions: pre.instructions,
            min_interval: u64::MAX,
            max_interval: 0,
        });
        if site.hits > 0 {
            let interval = pre.instructions.saturating_sub(site.last_instructions);
            site.min_interval = site.min_interval.min(interval);
            site.max_interval = site.max_interval.max(interval);
        }
        site.last_instructions = pre.instructions;
        site.hits += 1;
        site.blocks_killed += u64::from(action.blocks_killed);
        site.narrow_kills += u64::from(action.narrow_kills);
        site.wholesale += u64::from(action.wholesale);
        site.newly_hot += u64::from(action.newly_hot);
        // A site whose class or identity varies across hits keeps its FIRST observation in the row
        // above and is otherwise only visible in the class totals. See the module docs: this is
        // why a row is not a per-hit invariance claim.
        if let Some((_, insn)) = pre.covering
            && !site.values.contains(&insn.imm)
        {
            if site.values.len() < MAX_VALUES_PER_SITE {
                site.values.push(insn.imm);
            } else {
                site.values_overflow = true;
            }
        }
    }

    /// The report, one line per element. Sites are ranked by hit count.
    pub(crate) fn report_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "smc_trace events={} distinct_sites={}",
            self.events,
            self.sites.len()
        )];
        for (index, name) in CLASS_LABELS.iter().enumerate() {
            let events = self.class_events[index];
            let share = if self.events == 0 {
                0.0
            } else {
                100.0 * events as f64 / self.events as f64
            };
            lines.push(format!(
                "smc_class {name} events={events} share={share:.4} blocks_killed={} \
                 narrow_kills={} wholesale={} newly_hot={}",
                self.class_blocks_killed[index],
                self.class_narrow_kills[index],
                self.class_wholesale[index],
                self.class_newly_hot[index],
            ));
        }
        let mut shapes: Vec<(u64, u64)> = self.shape_events.iter().map(|(k, v)| (*k, *v)).collect();
        // Events first, then the packed key, so the ranking is stable across runs.
        shapes.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        lines.push(
            "smc_shape rank events class opcode modrm_reg insn_len imm_len disp_len operand_bytes"
                .to_string(),
        );
        for (rank, (key, events)) in shapes.iter().take(24).enumerate() {
            let modrm_reg = (key >> 32) & 0xFF;
            lines.push(format!(
                "smc_shape {rank} {events} {} {:#06x} {} {} {} {} {}",
                CLASS_LABELS[((key >> 56) & 0xFF) as usize],
                (key >> 40) & 0xFFFF,
                if modrm_reg == 0xFF {
                    "-".to_string()
                } else {
                    modrm_reg.to_string()
                },
                (key >> 24) & 0xFF,
                (key >> 16) & 0xFF,
                (key >> 8) & 0xFF,
                key & 0xFF,
            ));
        }
        let mut disp_store: Vec<(&u32, &DispStoreRow)> = self.disp_store_rows.iter().collect();
        // Events first, then the packed key, so the ranking is stable across runs -- the
        // `smc_shape` convention one table up.
        disp_store.sort_by(|a, b| b.1.events.cmp(&a.1.events).then(a.0.cmp(b.0)));
        lines.push(
            "smc_disp_store rank arm opcode modrm_reg disp_len prefixes admissible events blocks_killed narrow_kills newly_hot"
                .to_string(),
        );
        for (rank, (key, row)) in disp_store.iter().enumerate() {
            let opcode = (*key >> 16) as u16;
            let modrm_reg = (*key >> 8) & 0xFF;
            let disp_len = (*key >> 1) & 0x7F;
            let default_prefixes = *key & 1 != 0;
            // The matchers' two static bars, restated as a column so a reader does not have to
            // recompute them: a cell is admissible to its arm only at disp32 with no prefixes.
            // (`imm_len == 0` and the memory-form test are implied -- no `0x88`/`0x89`/`0x8B`
            // encoding carries an immediate, and a register form has no displacement field to
            // classify as one.) The arm's KNOB is a separate question and is not in this table.
            let admissible = disp_len == 4 && default_prefixes;
            lines.push(format!(
                "smc_disp_store {rank} {} {opcode:#06x} {} {disp_len} {} {} {} {} {} {}",
                disp_store_arm(opcode).unwrap_or("-"),
                if modrm_reg == 0xFF {
                    "-".to_string()
                } else {
                    modrm_reg.to_string()
                },
                if default_prefixes { "none" } else { "other" },
                if admissible { "yes" } else { "no" },
                row.events,
                row.blocks_killed,
                row.narrow_kills,
                row.newly_hot,
            ));
        }
        let mut ranked: Vec<&Site> = self.sites.values().collect();
        // Hits first, then the write address, so the ranking is stable across runs.
        ranked.sort_by(|a, b| {
            b.hits
                .cmp(&a.hits)
                .then(a.write_physical.cmp(&b.write_physical))
        });
        lines.push(
            "smc_site rank hits class write_phys width insn_phys insn_len opcode modrm \
             operand_bytes imm_len disp_len field_offset distinct_values blocks_killed \
             narrow_kills wholesale newly_hot min_interval max_interval"
                .to_string(),
        );
        for (rank, site) in ranked.iter().take(REPORT_SITES).enumerate() {
            lines.push(Self::site_line(rank, site));
        }
        lines
    }

    fn site_line(rank: usize, site: &Site) -> String {
        let (insn_phys, insn_len, opcode, modrm, operand_bytes, imm_len, disp_len) =
            match site.identity {
                Some(identity) => (
                    format!("{:#010x}", identity.start),
                    identity.len.to_string(),
                    format!("{:#06x}", identity.opcode),
                    match identity.modrm {
                        Some((mode, reg, rm)) => format!("mod{mode}/reg{reg}/rm{rm}"),
                        None => "-".to_string(),
                    },
                    identity.operand_bytes.to_string(),
                    identity.imm_len.to_string(),
                    identity.disp_len.to_string(),
                ),
                None => (
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                    "-".to_string(),
                ),
            };
        let values = if site.values_overflow {
            format!("{}+", site.values.len())
        } else {
            site.values.len().to_string()
        };
        let min_interval = if site.min_interval == u64::MAX {
            0
        } else {
            site.min_interval
        };
        format!(
            "smc_site {rank} {hits} {class} {write_phys:#010x} {width} {insn_phys} {insn_len} \
             {opcode} {modrm} {operand_bytes} {imm_len} {disp_len} {field_offset} {values} \
             {blocks_killed} {narrow_kills} {wholesale} {newly_hot} {min_interval} {max_interval}",
            hits = site.hits,
            class = site.class.label(),
            write_phys = site.write_physical,
            width = site.width,
            field_offset = site.field_offset,
            blocks_killed = site.blocks_killed,
            narrow_kills = site.narrow_kills,
            wholesale = site.wholesale,
            newly_hot = site.newly_hot,
            max_interval = site.max_interval,
        )
    }
}

/// Non-architectural slot holding the optional SMC trace, mirroring `UnitSimSlot`: always-equal,
/// clones disabled, prints opaquely, so enabling a diagnostic never makes two otherwise-identical
/// CPUs compare unequal.
#[derive(Default)]
pub(crate) struct SmcTraceSlot(pub(crate) Option<Box<SmcTrace>>);

impl PartialEq for SmcTraceSlot {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for SmcTraceSlot {}

impl Clone for SmcTraceSlot {
    fn clone(&self) -> Self {
        Self(None)
    }
}

impl std::fmt::Debug for SmcTraceSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmcTraceSlot")
            .field("enabled", &self.0.is_some())
            .finish()
    }
}

#[cfg(test)]
#[path = "smc_trace_test.rs"]
mod tests;
