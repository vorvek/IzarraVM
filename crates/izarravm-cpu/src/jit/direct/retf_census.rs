// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! The stage-0 RETF TARGET-ARITY census (design §5.0a). **THROWAWAY.**
//!
//! It exists to answer one question before the far-return ladder runs: what fraction of RETF
//! EXECUTIONS come from a site with at most two distinct targets, which is the two-cell PIC's
//! exact capacity. That fraction (`A2` in the design's §5.1) turns "entries fall" from a hope into
//! a pre-registered number, and it decides whether a large `jit_direct_far_link_refused_cs`
//! reading should be read as the chain being unavailable or as the arity being high.
//!
//! **EXECUTION-WEIGHTED, not site-weighted, and that is the whole point.** A site-weighted
//! histogram answers "how many RETF ADDRESSES are monomorphic", which is the wrong question: one
//! hot epilogue called from twenty sites can dominate a run while being one row.
//!
//! Compiled out unless `--features retf-arity-census`, then armed with
//! `IZARRAVM_RETF_ARITY_CENSUS=1`. It sits on the interpreter's `0xCA`/`0xCB` arm, which on
//! wolf3d-586 fires 274 M times per run, so both gates are load-bearing: the feature keeps a
//! shipped binary from carrying the map at all, and the runtime arm keeps a census BUILD from
//! paying for it on a leg that did not ask.
//!
//! It does not ship. Delete it, or leave it behind this feature, before the slice merges.

use std::collections::HashMap;

use super::BlockCache;

/// How many distinct target linears one site records before it saturates. A saturated site reports
/// "at least 8" and must never be read as exactly 8. Two is the number that matters; eight is
/// enough shape to tell "three or four" from "a jump table".
pub(crate) const RETF_TARGET_CENSUS_CAP: usize = 8;

#[derive(Clone, Copy)]
struct RetfSiteRecord {
    targets: [u32; RETF_TARGET_CENSUS_CAP],
    target_count: u8,
    saturated: bool,
    executions: u64,
}

impl Default for RetfSiteRecord {
    fn default() -> Self {
        Self {
            targets: [0; RETF_TARGET_CENSUS_CAP],
            target_count: 0,
            saturated: false,
            executions: 0,
        }
    }
}

#[derive(Default)]
pub(crate) struct RetfArityCensus {
    sites: HashMap<u64, RetfSiteRecord>,
}

impl RetfArityCensus {
    /// Record one RETF execution.
    ///
    /// `site` keys the RETF instruction, `v86` splits real mode from V86 so §6.1's "the `v86` arm
    /// captures ~100% of wolf3d's population" becomes a number rather than an inference, and
    /// `target_linear` is `(selector << 4) + ip` -- exactly the quantity the far cell will hold, so
    /// the arity measured here is the arity the PIC will see.
    pub(crate) fn note(&mut self, site: u32, v86: bool, target_linear: u32) {
        let key = u64::from(site) | (u64::from(v86) << 32);
        let record = self.sites.entry(key).or_default();
        record.executions += 1;
        let live = usize::from(record.target_count);
        if record.targets[..live].contains(&target_linear) {
            return;
        }
        if live == RETF_TARGET_CENSUS_CAP {
            record.saturated = true;
            return;
        }
        record.targets[live] = target_linear;
        record.target_count += 1;
    }

    /// EXECUTION-WEIGHTED histogram: cell `n` holds the number of RETF EXECUTIONS taken at sites
    /// with exactly `n` distinct targets, and the last cell holds the saturated sites' executions.
    ///
    /// Cell 0 is always zero -- a site with a record has executed at least once and therefore has
    /// at least one target -- and is kept so the index reads as the distinct count.
    pub(crate) fn histogram(&self) -> [u64; RETF_TARGET_CENSUS_CAP + 2] {
        let mut histogram = [0u64; RETF_TARGET_CENSUS_CAP + 2];
        for record in self.sites.values() {
            let cell = if record.saturated {
                RETF_TARGET_CENSUS_CAP + 1
            } else {
                usize::from(record.target_count)
            };
            histogram[cell] += record.executions;
        }
        histogram
    }

    /// How many distinct (site, mode) rows the census holds, so a reader can tell a histogram
    /// built from four sites from one built from forty thousand.
    pub(crate) fn sites(&self) -> u64 {
        self.sites.len() as u64
    }
}

/// `Some` only when the feature is compiled in AND `IZARRAVM_RETF_ARITY_CENSUS=1` is exported.
/// See `direct_link_refusal_census_default` for the shape.
pub(crate) fn retf_arity_census_default() -> Option<Box<RetfArityCensus>> {
    std::env::var("IZARRAVM_RETF_ARITY_CENSUS")
        .is_ok_and(|value| value.trim() == "1")
        .then(|| Box::new(RetfArityCensus::default()))
}

impl BlockCache {
    /// Record one INTERPRETED far return in the stage-0 arity census, when it is armed.
    ///
    /// Gated at the CALL SITE as well as here, per `default-off-instruments-tax-hot-path`: this
    /// fires 274 M times on a wolf3d run and a disarmed build must pay one null test, not a call.
    pub(crate) fn note_retf_target(&mut self, site: u32, v86: bool, target_linear: u32) {
        if let Some(census) = self.retf_arity_census.as_mut() {
            census.note(site, v86, target_linear);
        }
    }

    pub(crate) fn retf_arity_census_active(&self) -> bool {
        self.retf_arity_census.is_some()
    }

    /// `(execution-weighted histogram, distinct sites)`, or `None` when the census is disarmed.
    pub(crate) fn retf_arity_snapshot(&self) -> Option<([u64; RETF_TARGET_CENSUS_CAP + 2], u64)> {
        self.retf_arity_census
            .as_ref()
            .map(|census| (census.histogram(), census.sites()))
    }
}
