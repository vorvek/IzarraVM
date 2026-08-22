// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Cold, opt-in attribution for the interpreter call-out helpers.
//!
//! FOUR of them since S2, and this file did not learn about the fourth until the S4 review round:
//! `CallOutHelper::InterpretOne` was added with the generic call-out and every `match` here kept
//! its three arms, so `cargo build --all-features` had not compiled since. The four-arm shape is
//! restored below and `--all-features` is a build gate from now on -- a feature nobody builds is a
//! feature that rots, and this one is the attribution instrument the call-out campaign reaches for
//! whenever a helper's outcome mix is in question.

use super::{CallOutHelper, InterpretOneRow};
use crate::{
    DirectCallOutAttributionHelperRow, DirectCallOutAttributionPortRow,
    DirectCallOutAttributionSnapshot, DirectCallOutOutcomeCounts, DirectStallSnapshot,
};

const HELPER_COUNT: usize = 4;
const PORT_COUNT: usize = 1 << 16;

#[derive(Clone, Copy)]
pub(crate) enum CallOutOutcome {
    Continued,
    StepBreak,
    Abnormal,
}

impl CallOutHelper {
    fn attribution_index(self) -> usize {
        match self {
            Self::PortReadAlDx => 0,
            Self::PushAllDword => 1,
            Self::PopAllDword => 2,
            // ONE index for the whole `InterpretOne` family, not one per row. This instrument is
            // about the HELPERS -- which of them is being called and how its calls end -- and the
            // per-row split already exists as `callout_interpret_one_rows`, which is ungated and
            // carries executed, resync, resync_fault and demoted. A second row axis here would be
            // the same census under a feature flag.
            Self::InterpretOne { .. } => 3,
        }
    }

    fn attribution_label(self) -> &'static str {
        match self {
            Self::PortReadAlDx => "in_al_dx",
            Self::PushAllDword => "pushad",
            Self::PopAllDword => "popad",
            Self::InterpretOne { .. } => "interpret_one",
        }
    }
}

impl DirectCallOutOutcomeCounts {
    fn note(&mut self, outcome: CallOutOutcome) {
        self.attempts = self
            .attempts
            .checked_add(1)
            .expect("Direct call-out attempt count overflowed");
        let counter = match outcome {
            CallOutOutcome::Continued => &mut self.continued,
            CallOutOutcome::StepBreak => &mut self.step_break,
            CallOutOutcome::Abnormal => &mut self.abnormal,
        };
        *counter = counter
            .checked_add(1)
            .expect("Direct call-out outcome count overflowed");
    }

    fn checked_add(self, other: Self) -> Self {
        Self {
            attempts: self
                .attempts
                .checked_add(other.attempts)
                .expect("Direct call-out attempt sum overflowed"),
            continued: self
                .continued
                .checked_add(other.continued)
                .expect("Direct call-out continued sum overflowed"),
            step_break: self
                .step_break
                .checked_add(other.step_break)
                .expect("Direct call-out step-break sum overflowed"),
            abnormal: self
                .abnormal
                .checked_add(other.abnormal)
                .expect("Direct call-out abnormal sum overflowed"),
        }
    }

    fn assert_closed(self) {
        let outcomes = self
            .continued
            .checked_add(self.step_break)
            .and_then(|sum| sum.checked_add(self.abnormal))
            .expect("Direct call-out outcome closure overflowed");
        assert_eq!(self.attempts, outcomes, "Direct call-out row did not close");
    }
}

pub(crate) struct CallOutAttribution {
    helpers: [DirectCallOutOutcomeCounts; HELPER_COUNT],
    ports: Box<[DirectCallOutOutcomeCounts]>,
}

impl Default for CallOutAttribution {
    fn default() -> Self {
        Self {
            helpers: [DirectCallOutOutcomeCounts::default(); HELPER_COUNT],
            ports: vec![DirectCallOutOutcomeCounts::default(); PORT_COUNT].into_boxed_slice(),
        }
    }
}

impl CallOutAttribution {
    pub(super) fn note(
        &mut self,
        helper: CallOutHelper,
        port: Option<u16>,
        outcome: CallOutOutcome,
    ) {
        self.helpers[helper.attribution_index()].note(outcome);
        match helper {
            CallOutHelper::PortReadAlDx => {
                self.ports[usize::from(port.expect("IN AL,DX attribution needs its port"))]
                    .note(outcome);
            }
            // `InterpretOne` joins the no-port arm rather than getting one of its own: it reaches
            // no port at all, which is exactly what the existing assertion says. The one call-out
            // that touches a port is `0xEC`, and it is the arm above.
            CallOutHelper::PushAllDword
            | CallOutHelper::PopAllDword
            | CallOutHelper::InterpretOne { .. } => {
                debug_assert!(port.is_none(), "memory helper attribution carried a port");
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn snapshot(&self) -> DirectCallOutAttributionSnapshot {
        let helper_kinds = [
            CallOutHelper::PortReadAlDx,
            CallOutHelper::PushAllDword,
            CallOutHelper::PopAllDword,
            // The row carried here is arbitrary and unused: `attribution_index` and
            // `attribution_label` both match `InterpretOne { .. }` without reading it, because
            // this axis is the helper and not the row. `PopRm` is the family's first member.
            CallOutHelper::InterpretOne {
                row: InterpretOneRow::PopRm,
            },
        ];
        let helpers = helper_kinds
            .into_iter()
            .zip(self.helpers.iter().copied())
            .map(|(helper, counts)| {
                counts.assert_closed();
                DirectCallOutAttributionHelperRow {
                    helper: helper.attribution_label(),
                    counts,
                }
            })
            .collect::<Vec<_>>();
        let totals = self
            .helpers
            .iter()
            .copied()
            .fold(DirectCallOutOutcomeCounts::default(), |sum, row| {
                sum.checked_add(row)
            });
        totals.assert_closed();

        let mut port_totals = DirectCallOutOutcomeCounts::default();
        let ports = self
            .ports
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(port, counts)| {
                counts.assert_closed();
                port_totals = port_totals.checked_add(counts);
                (counts.attempts != 0).then(|| DirectCallOutAttributionPortRow {
                    port: u16::try_from(port).expect("dense port index must fit u16"),
                    counts,
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(port_totals, self.helpers[0], "IN AL,DX ports did not close");

        DirectCallOutAttributionSnapshot {
            helpers,
            ports,
            totals,
        }
    }
}

pub(crate) fn direct_callout_attribution_default() -> Option<Box<CallOutAttribution>> {
    matches!(
        std::env::var("IZARRAVM_DIRECT_CALLOUT_ATTRIBUTION").as_deref(),
        Ok("1")
    )
    .then(|| Box::new(CallOutAttribution::default()))
}

impl crate::jit::JitState {
    pub(crate) fn note_callout_attribution(
        &mut self,
        helper: CallOutHelper,
        port: Option<u16>,
        outcome: CallOutOutcome,
    ) {
        if let Some(attribution) = self.direct_callout_attribution.as_mut() {
            attribution.note(helper, port, outcome);
        }
    }

    #[cfg(test)]
    pub(crate) fn enable_direct_callout_attribution_for_test(&mut self) {
        let stalls = self.stall_snapshot();
        assert_eq!(stalls.callout_executed, 0);
        assert_eq!(stalls.side_exit_callout_step_break, 0);
        assert_eq!(stalls.side_exit_callout_abnormal, 0);
        self.direct_callout_attribution = Some(Box::new(CallOutAttribution::default()));
    }

    #[cold]
    #[inline(never)]
    pub(crate) fn direct_callout_attribution_snapshot(
        &self,
    ) -> Option<DirectCallOutAttributionSnapshot> {
        let snapshot = self.direct_callout_attribution.as_ref()?.snapshot();
        let DirectStallSnapshot {
            callout_executed,
            side_exit_callout_abnormal,
            side_exit_callout_step_break,
            ..
        } = self.stall_snapshot();
        assert_eq!(snapshot.totals.attempts, callout_executed);
        assert_eq!(snapshot.totals.abnormal, side_exit_callout_abnormal);
        assert_eq!(snapshot.totals.step_break, side_exit_callout_step_break);
        Some(snapshot)
    }
}
