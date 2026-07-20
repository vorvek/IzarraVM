// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalSectionRequirement, CanonicalStateError};
use izarravm_cpu::{CanonicalCpuExecution, CpuCanonicalCaptureError, bus_timing};
use thiserror::Error;

use crate::{
    CacheModel, Machine,
    cache_config::{
        CACHE_L1_MAX_LINES, CACHE_L2_MAX_LINES, CACHE_LINE_BYTES, CACHE_TIER_DISABLED_MASK,
        cache_level_config, code_fetch_ws, tier_cost,
    },
    dma::{CanonicalDma8237Pair, CanonicalDmaEventTotalsV1},
    pic::CanonicalPic8259Pair,
    pit::CanonicalPit,
    rtc::CanonicalRtc,
    speaker::CanonicalSpeaker,
    timeline::{CanonicalTimelineError, CanonicalTimelineProjection},
    unittester::CanonicalUnitTester,
};

const STATE_SNAPSHOT_V1_SCHEMA_NAMESPACE: u32 = 0x0000_0000;
const STATE_SNAPSHOT_V1_CPU_NAMESPACE: u32 = 0x0001_0000;
const STATE_SNAPSHOT_V1_MACHINE_NAMESPACE: u32 = 0x0002_0000;
const STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK: u32 = 0xffff_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StateSnapshotV1FoundationSection {
    id: u32,
    version: u16,
    requirement: CanonicalSectionRequirement,
}

/// Stable, non-exhaustive prefix of the StateSnapshotV1 section namespace.
///
/// Machine, memory, device, media, audio, and video sections will extend this
/// list before the complete-schema validator or any snapshot artifact exists.
const STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS: [StateSnapshotV1FoundationSection; 13] = [
    StateSnapshotV1FoundationSection {
        id: 0x0000_0001,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0001_0001,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0001_0002,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0001_0003,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0001,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0002,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0003,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0004,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0005,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0006,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0007,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0008,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_0009,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
];

const _: () = {
    let sections = STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS;
    assert!(
        sections[0].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_SCHEMA_NAMESPACE
    );
    assert!(
        sections[1].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
    );
    assert!(
        sections[2].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
    );
    assert!(
        sections[3].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
    );
    assert!(
        sections[4].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[5].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[6].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[7].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[8].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[9].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[10].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[11].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[12].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(sections[0].id < sections[1].id);
    assert!(sections[1].id < sections[2].id);
    assert!(sections[2].id < sections[3].id);
    assert!(sections[3].id < sections[4].id);
    assert!(sections[4].id < sections[5].id);
    assert!(sections[5].id < sections[6].id);
    assert!(sections[6].id < sections[7].id);
    assert!(sections[7].id < sections[8].id);
    assert!(sections[8].id < sections[9].id);
    assert!(sections[9].id < sections[10].id);
    assert!(sections[10].id < sections[11].id);
    assert!(sections[11].id < sections[12].id);
};

/// A machine boundary that cannot yet be represented by canonical owner payloads.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MachineCanonicalCaptureError {
    #[error(transparent)]
    Cpu(#[from] CpuCanonicalCaptureError),
    #[error("unit-tester command {command:#04x} has not been serviced")]
    PendingUnitTesterCommand { command: u8 },
    #[error("a CPU mode change has not been serviced")]
    PendingModeChange,
    #[error("Toka service command {command:#04x} has not been serviced")]
    PendingTokaService { command: u8 },
    #[error("a BIOS32 service has not been serviced")]
    PendingBios32Service,
    #[error("an exact device-memory write has not been published to the CPU")]
    PendingDeviceMemoryWriteRange,
    #[error("a coarse device-memory write has not been published to the CPU")]
    PendingDeviceMemoryWrite,
    #[error("a full direct-map change has not been published to the CPU")]
    PendingDirectMapChange,
    #[error("a data-only direct-map change has not been published to the CPU")]
    PendingDirectDataMapChange,
    #[error("{clocks} ISA batch clocks have not been committed")]
    UncommittedBatchTiming { clocks: u64 },
    #[error("{pending} console bytes have not been published to the display")]
    PendingConsolePublication { pending: usize },
    #[error("console display tracker {shown} exceeds the {total} output bytes")]
    InvalidConsoleTracker { shown: usize, total: usize },
    #[error("machine mode {machine} disagrees with CPU mode {cpu}")]
    InconsistentCpuMode {
        machine: izarravm_core::GswMode,
        cpu: izarravm_core::GswMode,
    },
    #[error("timeline CPU quantum is {actual}, expected {expected}")]
    InconsistentTimelineQuantum { expected: u64, actual: u64 },
    #[error("timeline {phase} remainder {remainder} is not below {limit}")]
    InvalidTimelineRemainder {
        phase: &'static str,
        remainder: u64,
        limit: u64,
    },
    #[error("timeline I/O-stall total {io_stall_ticks} exceeds master total {now_ticks}")]
    InvalidTimelineTotals { now_ticks: u64, io_stall_ticks: u64 },
    #[error("halted total {halted_ticks} exceeds master total {now_ticks}")]
    InvalidHaltedTicks { halted_ticks: u64, now_ticks: u64 },
    #[error("I/O-stall total {io_stall_clocks} exceeds elapsed total {elapsed_clocks}")]
    InvalidIoStallClocks {
        io_stall_clocks: u64,
        elapsed_clocks: u64,
    },
    #[error("bus-scaler remainder {remainder} is not below denominator {denominator}")]
    InvalidBusRemainder { remainder: u64, denominator: u32 },
    #[error("modeled-cache {tier} storage has {actual} entries, expected {expected}")]
    InvalidModeledCacheStorageLength {
        tier: &'static str,
        expected: usize,
        actual: usize,
    },
    #[error(
        "modeled-cache masks are L1 {actual_l1:#010x}, L2 {actual_l2:#010x}; expected L1 {expected_l1:#010x}, L2 {expected_l2:#010x}"
    )]
    InconsistentModeledCacheConfiguration {
        expected_l1: u32,
        actual_l1: u32,
        expected_l2: u32,
        actual_l2: u32,
    },
    #[error("modeled-cache costs are {actual:?}; expected {expected:?} for L1, L2, and RAM")]
    InconsistentModeledCacheCosts { expected: [u8; 3], actual: [u8; 3] },
    #[error("modeled code-fetch wait states are {actual}, expected {expected}")]
    InconsistentModeledCodeFetchWaitStates { expected: u8, actual: u8 },
    #[error("RAM byte length overflowed for a {memory_mib} MiB machine")]
    RamLengthOverflow { memory_mib: u16 },
    #[error("RAM backing has {actual} bytes, expected {expected}")]
    InconsistentRamLength { expected: usize, actual: usize },
    #[error("system ROM backing has {actual} bytes, expected {expected}")]
    InconsistentSystemRomLength { expected: usize, actual: usize },
    #[error("the derived RAM page lookup is inconsistent with RAM and video decode")]
    InconsistentRamPageLookup,
}

impl From<CanonicalTimelineError> for MachineCanonicalCaptureError {
    fn from(error: CanonicalTimelineError) -> Self {
        match error {
            CanonicalTimelineError::CpuQuantum { expected, actual } => {
                Self::InconsistentTimelineQuantum { expected, actual }
            }
            CanonicalTimelineError::PhaseRemainder {
                phase,
                remainder,
                limit,
            } => Self::InvalidTimelineRemainder {
                phase,
                remainder,
                limit,
            },
            CanonicalTimelineError::Totals {
                now_ticks,
                io_stall_ticks,
            } => Self::InvalidTimelineTotals {
                now_ticks,
                io_stall_ticks,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CanonicalMachineControl {
    memory_mib: u16,
    wait_states: [u8; 4],
    fast_post: bool,
    pending_soft_int: Option<u8>,
    last_int_vector: Option<u8>,
    timeline: CanonicalTimelineProjection,
    elapsed_clocks: u64,
    io_stall_clocks: u64,
    halted_ticks: u64,
    raw_bus_clocks: u64,
    scaled_bus_clocks: u64,
    bus_rem: u64,
}

fn write_optional_u8(
    out: &mut CanonicalFieldWriter<'_>,
    value: Option<u8>,
) -> Result<(), CanonicalStateError> {
    out.write_bool(value.is_some())?;
    out.write_u8(value.unwrap_or(0))
}

impl CanonicalMachineControl {
    fn write_payload(&self, out: &mut CanonicalFieldWriter<'_>) -> Result<(), CanonicalStateError> {
        out.write_u16(self.memory_mib)?;
        for wait_states in self.wait_states {
            out.write_u8(wait_states)?;
        }
        out.write_bool(self.fast_post)?;
        write_optional_u8(out, self.pending_soft_int)?;
        write_optional_u8(out, self.last_int_vector)?;
        self.timeline.write_payload(out)?;
        out.write_u64(self.elapsed_clocks)?;
        out.write_u64(self.io_stall_clocks)?;
        out.write_u64(self.halted_ticks)?;
        out.write_u64(self.raw_bus_clocks)?;
        out.write_u64(self.scaled_bus_clocks)?;
        out.write_u64(self.bus_rem)
    }
}

const MAX_MODELED_CACHE_LINE: u32 = u32::MAX / CACHE_LINE_BYTES;

#[derive(Debug, Clone, Copy)]
struct CanonicalModeledCacheProjection<'a> {
    cache: &'a CacheModel,
    l1_mask: u32,
    l2_mask: u32,
    flat_data_cost: bool,
}

fn effective_modeled_cache_tags(tags: &[u32], mask: u32) -> Result<Vec<u32>, CanonicalStateError> {
    if mask == CACHE_TIER_DISABLED_MASK {
        return Ok(Vec::new());
    }
    let active_len = usize::try_from(mask)
        .ok()
        .and_then(|mask| mask.checked_add(1))
        .ok_or(CanonicalStateError::LengthOverflow)?;
    let mut effective = Vec::new();
    effective
        .try_reserve_exact(active_len)
        .map_err(|_| CanonicalStateError::AllocationFailed)?;
    for (slot, &tag) in tags[..active_len].iter().enumerate() {
        if tag != super::CACHE_EMPTY_TAG
            && tag <= MAX_MODELED_CACHE_LINE
            && tag & mask == slot as u32
        {
            effective.push(tag);
        }
    }
    effective.sort_unstable();
    Ok(effective)
}

impl CanonicalModeledCacheProjection<'_> {
    fn write_payload(&self, out: &mut CanonicalFieldWriter<'_>) -> Result<(), CanonicalStateError> {
        if self.flat_data_cost {
            out.write_count(0)?;
            return out.write_count(0);
        }

        let l1_tags = effective_modeled_cache_tags(&self.cache.l1_tags, self.l1_mask)?;
        let l2_tags = effective_modeled_cache_tags(&self.cache.l2_tags, self.l2_mask)?;
        out.write_count(
            u64::try_from(l1_tags.len()).map_err(|_| CanonicalStateError::LengthOverflow)?,
        )?;
        for tag in l1_tags {
            out.write_u32(tag)?;
        }
        out.write_count(
            u64::try_from(l2_tags.len()).map_err(|_| CanonicalStateError::LengthOverflow)?,
        )?;
        for tag in l2_tags {
            out.write_u32(tag)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct CanonicalRamRomProjection<'a> {
    ram: &'a [u8],
    rom: &'a [u8],
}

impl CanonicalRamRomProjection<'_> {
    fn write_payload(&self, out: &mut CanonicalFieldWriter<'_>) -> Result<(), CanonicalStateError> {
        out.write_len_prefixed_bytes(self.ram)?;
        out.write_len_prefixed_bytes(self.rom)
    }
}

impl CacheModel {
    fn canonical_projection(
        &self,
        mode: izarravm_core::GswMode,
    ) -> Result<CanonicalModeledCacheProjection<'_>, MachineCanonicalCaptureError> {
        if self.l1_tags.len() != CACHE_L1_MAX_LINES {
            return Err(
                MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
                    tier: "L1",
                    expected: CACHE_L1_MAX_LINES,
                    actual: self.l1_tags.len(),
                },
            );
        }
        if self.l2_tags.len() != CACHE_L2_MAX_LINES {
            return Err(
                MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
                    tier: "L2",
                    expected: CACHE_L2_MAX_LINES,
                    actual: self.l2_tags.len(),
                },
            );
        }

        let expected_config = cache_level_config(mode);
        if self.config.l1_mask != expected_config.l1_mask
            || self.config.l2_mask != expected_config.l2_mask
        {
            return Err(
                MachineCanonicalCaptureError::InconsistentModeledCacheConfiguration {
                    expected_l1: expected_config.l1_mask,
                    actual_l1: self.config.l1_mask,
                    expected_l2: expected_config.l2_mask,
                    actual_l2: self.config.l2_mask,
                },
            );
        }

        let expected_cost = tier_cost(mode);
        let expected_costs = [expected_cost.l1, expected_cost.l2, expected_cost.ram];
        let actual_costs = [self.cost.l1, self.cost.l2, self.cost.ram];
        if actual_costs != expected_costs {
            return Err(
                MachineCanonicalCaptureError::InconsistentModeledCacheCosts {
                    expected: expected_costs,
                    actual: actual_costs,
                },
            );
        }

        let expected_code_fetch_ws = code_fetch_ws(mode);
        if self.code_fetch_ws != expected_code_fetch_ws {
            return Err(
                MachineCanonicalCaptureError::InconsistentModeledCodeFetchWaitStates {
                    expected: expected_code_fetch_ws,
                    actual: self.code_fetch_ws,
                },
            );
        }

        Ok(CanonicalModeledCacheProjection {
            cache: self,
            l1_mask: expected_config.l1_mask,
            l2_mask: expected_config.l2_mask,
            flat_data_cost: mode.uses_approximate_timing(),
        })
    }
}

/// Immutable proof that the machine is structurally ready for canonical capture.
///
/// This token does not attest semantic completion or a particular stop reason.
/// The campaign caller must later bind it to the locally returned TestExit code
/// after wall timing has stopped. No StateSnapshotV1 bytes can be emitted until
/// every required owner payload and the complete-schema validator exist.
#[must_use]
pub struct CanonicalMachineStateCapture<'a> {
    // Retained for the future owner composer. Leading underscores keep this
    // foundation slice free of accessors that could expose a partial snapshot.
    _machine: &'a Machine,
    _cpu_execution: CanonicalCpuExecution<'a>,
    machine_control: CanonicalMachineControl,
    modeled_cache: CanonicalModeledCacheProjection<'a>,
    ram_rom: CanonicalRamRomProjection<'a>,
    pic: CanonicalPic8259Pair<'a>,
    pit: CanonicalPit<'a>,
    dma: CanonicalDma8237Pair<'a>,
    dma_event_totals: CanonicalDmaEventTotalsV1<'a>,
    rtc: CanonicalRtc<'a>,
    unit_tester: CanonicalUnitTester<'a>,
    speaker: CanonicalSpeaker<'a>,
}

impl CanonicalMachineStateCapture<'_> {
    /// Writes the required Machine control/timing owner payload.
    ///
    /// The complete StateSnapshotV1 composer remains unavailable until every
    /// required owner section has landed.
    #[allow(dead_code)]
    pub(crate) fn write_machine_control_timing_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.machine_control.write_payload(out)
    }

    /// Writes the required Machine modeled-cache owner payload.
    ///
    /// Only tag state that can change future guest timing is represented. The
    /// complete StateSnapshotV1 composer remains unavailable until every
    /// required owner section has landed.
    #[allow(dead_code)]
    pub(crate) fn write_modeled_cache_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.modeled_cache.write_payload(out)
    }

    /// Writes the required Machine RAM and system-ROM owner payload.
    ///
    /// Both authoritative stores are copied in raw backing order. Bus aliases,
    /// device apertures, and derived host mappings are represented by their own
    /// owners rather than projected into this payload.
    #[allow(dead_code)]
    pub(crate) fn write_ram_rom_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.ram_rom.write_payload(out)
    }

    /// Writes the required Machine PIC-pair owner payload.
    ///
    /// The payload retains every behaviorally effective interrupt-controller
    /// latch while projecting out ICW bits that this model never consumes.
    #[allow(dead_code)]
    pub(crate) fn write_pic_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.pic.write_payload(out)
    }

    /// Writes the required Machine PIT owner payload.
    ///
    /// The three fixed counter records preserve live timing and destructive
    /// read state without duplicating the PIT clock phase owned by the timeline.
    #[allow(dead_code)]
    pub(crate) fn write_pit_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.pit.write_payload(out)
    }

    /// Writes the required Machine DMA semantic-state owner payload.
    #[allow(dead_code)]
    pub(crate) fn write_dma_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.dma.write_payload(out)
    }

    /// Writes version 1 of the exact DMA event totals for CorrectnessOracle.
    ///
    /// This record is not a StateSnapshotV1 section and is never allowlisted as
    /// a JIT mechanism counter. The future oracle composer will enclose it in
    /// its own versioned deterministic-evidence artifact.
    #[allow(dead_code)]
    pub(crate) fn write_dma_event_totals_v1_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.dma_event_totals.write_payload(out)
    }

    /// Writes the required RTC and CMOS owner payload.
    ///
    /// The projection keeps guest-visible register and timing continuation
    /// state while excluding host seeding and persistence notifications.
    #[allow(dead_code)]
    pub(crate) fn write_rtc_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.rtc.write_payload(out)
    }

    /// Writes the required UnitTester register-file owner payload.
    ///
    /// Deferred commands are rejected before capture, so this projection holds
    /// only the guest-visible index and register bytes.
    #[allow(dead_code)]
    pub(crate) fn write_unit_tester_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.unit_tester.write_payload(out)
    }

    /// Writes the required PC speaker control-latch owner payload.
    ///
    /// PIT channel 2 owns the live OUT and GATE lines. This byte retains the
    /// two low port 0x61 bits exactly as the guest reads them back.
    #[allow(dead_code)]
    pub(crate) fn write_speaker_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.speaker.write_payload(out)
    }
}

impl Machine {
    /// Validates a read-only machine boundary for later canonical serialization.
    ///
    /// Pending interrupts and in-flight device operations are representable
    /// state and remain untouched. `io_touched` is batch-control scratch and is
    /// deliberately ignored. Deferred services, timing, and CPU-coherence work
    /// must already be committed before capture.
    pub fn canonical_state_capture(
        &self,
    ) -> Result<CanonicalMachineStateCapture<'_>, MachineCanonicalCaptureError> {
        if let Some(command) = self.unittester.pending_command() {
            return Err(MachineCanonicalCaptureError::PendingUnitTesterCommand { command });
        }
        if self.pending_mode.is_some() {
            return Err(MachineCanonicalCaptureError::PendingModeChange);
        }
        if let Some(command) = self.pending_toka_service {
            return Err(MachineCanonicalCaptureError::PendingTokaService { command });
        }
        if self.pending_bios32.is_some() {
            return Err(MachineCanonicalCaptureError::PendingBios32Service);
        }
        if self.pending_device_memory_write_range.is_some() {
            return Err(MachineCanonicalCaptureError::PendingDeviceMemoryWriteRange);
        }
        if self.device_wrote_memory {
            return Err(MachineCanonicalCaptureError::PendingDeviceMemoryWrite);
        }
        if self.direct_map_changed {
            return Err(MachineCanonicalCaptureError::PendingDirectMapChange);
        }
        if self.direct_data_map_changed {
            return Err(MachineCanonicalCaptureError::PendingDirectDataMapChange);
        }
        if self.isa_io_batch_clocks != 0 {
            return Err(MachineCanonicalCaptureError::UncommittedBatchTiming {
                clocks: self.isa_io_batch_clocks,
            });
        }
        let console_total = self.program_output.len();
        if self.dos_screen_shown < console_total {
            return Err(MachineCanonicalCaptureError::PendingConsolePublication {
                pending: console_total - self.dos_screen_shown,
            });
        }
        if self.dos_screen_shown > console_total {
            return Err(MachineCanonicalCaptureError::InvalidConsoleTracker {
                shown: self.dos_screen_shown,
                total: console_total,
            });
        }
        let cpu_mode = self.cpu.mode();
        if self.active_mode != cpu_mode {
            return Err(MachineCanonicalCaptureError::InconsistentCpuMode {
                machine: self.active_mode,
                cpu: cpu_mode,
            });
        }
        let modeled_cache = self.cache_model.canonical_projection(self.active_mode)?;
        let expected_ram_len = usize::from(self.profile.memory_mib)
            .checked_mul(1024 * 1024)
            .ok_or(MachineCanonicalCaptureError::RamLengthOverflow {
                memory_mib: self.profile.memory_mib,
            })?;
        let actual_ram_len = self.memory.len();
        if actual_ram_len != expected_ram_len {
            return Err(MachineCanonicalCaptureError::InconsistentRamLength {
                expected: expected_ram_len,
                actual: actual_ram_len,
            });
        }
        if self.rom.len() != super::BIOS_ROM_SIZE {
            return Err(MachineCanonicalCaptureError::InconsistentSystemRomLength {
                expected: super::BIOS_ROM_SIZE,
                actual: self.rom.len(),
            });
        }
        if !self.ram_lookup.is_consistent(actual_ram_len, &self.vega) {
            return Err(MachineCanonicalCaptureError::InconsistentRamPageLookup);
        }
        let ram_rom = CanonicalRamRomProjection {
            ram: self.memory.as_slice(),
            rom: self.rom.as_slice(),
        };
        let pic = self.pic.canonical_projection();
        let pit = self.pit.canonical_projection();
        let dma = self.dma.canonical_projection();
        let dma_event_totals = self.dma.canonical_event_totals_v1();
        let rtc = self.rtc.canonical_projection();
        let unit_tester = self.unittester.canonical_projection();
        let speaker = self.speaker.canonical_projection();
        let timeline = self.timeline.canonical_projection(self.active_mode)?;
        let now_ticks = self.timeline.now_ticks();
        if self.halted_ticks > now_ticks {
            return Err(MachineCanonicalCaptureError::InvalidHaltedTicks {
                halted_ticks: self.halted_ticks,
                now_ticks,
            });
        }
        if self.io_stall_clocks > self.elapsed_clocks {
            return Err(MachineCanonicalCaptureError::InvalidIoStallClocks {
                io_stall_clocks: self.io_stall_clocks,
                elapsed_clocks: self.elapsed_clocks,
            });
        }
        let denominator = bus_timing(self.active_mode.persona()).1;
        if self.bus_rem >= u64::from(denominator) {
            return Err(MachineCanonicalCaptureError::InvalidBusRemainder {
                remainder: self.bus_rem,
                denominator,
            });
        }
        let cpu_execution = self.cpu.canonical_execution_capture()?;
        let wait_states = self.profile.wait_states;
        let machine_control = CanonicalMachineControl {
            memory_mib: self.profile.memory_mib,
            wait_states: [
                wait_states.ram,
                wait_states.rom,
                wait_states.video,
                wait_states.io,
            ],
            fast_post: self.fast_post,
            pending_soft_int: self
                .cpu
                .is_ring0_protected()
                .then_some(self.pending_soft_int)
                .flatten(),
            last_int_vector: self.last_int_vector,
            timeline,
            elapsed_clocks: self.elapsed_clocks,
            io_stall_clocks: self.io_stall_clocks,
            halted_ticks: self.halted_ticks,
            raw_bus_clocks: self.trace.elapsed_clocks(),
            scaled_bus_clocks: self.scaled_bus_clocks,
            bus_rem: self.bus_rem,
        };
        Ok(CanonicalMachineStateCapture {
            _machine: self,
            _cpu_execution: cpu_execution,
            machine_control,
            modeled_cache,
            ram_rom,
            pic,
            pit,
            dma,
            dma_event_totals,
            rtc,
            unit_tester,
            speaker,
        })
    }
}

#[cfg(test)]
mod tests {
    use izarravm_bus::{BusAccessKind, BusWidth, CpuBus, TracingMode};
    use izarravm_core::{
        CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion,
        CanonicalStateView, CanonicalStateWriter, GswMode, MASTER_CLOCK_HZ, VideoCard,
    };
    use izarravm_cpu::CpuCanonicalCaptureError;
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    use izarravm_cpu::SegmentIndex;

    use super::*;
    use crate::{
        BIOS_ROM_SIZE, Bios32Call, MachineProfile, StopReason, WaitStateProfile, unittester,
    };

    const MACHINE_CONTROL_TIMING_PAYLOAD_LEN: usize = 163;
    const EMPTY_MODELED_CACHE_PAYLOAD_LEN: usize = 16;
    const PIC_PAYLOAD_LEN: usize = 34;
    const PIT_PAYLOAD_LEN: usize = 60;
    const DMA_PAYLOAD_LEN: usize = 152;
    const DMA_EVENT_TOTALS_V1_PAYLOAD_LEN: usize = 64;
    const RTC_PAYLOAD_LEN: usize = 82;
    const UNIT_TESTER_PAYLOAD_LEN: usize = 33;
    const SPEAKER_PAYLOAD_LEN: usize = 1;
    const PIT_COUNTER_PAYLOAD_LEN: usize = PIT_PAYLOAD_LEN / 3;
    const PIT_CHANNEL_2_GATE_OFFSET: usize = 2 * PIT_COUNTER_PAYLOAD_LEN + 10;
    const TEST_DMA_EVENT_TOTALS_ENVELOPE_ID: u32 = 0x7ffe_0001;
    const TEST_MEMORY_MIB: u16 = 2;

    fn test_machine() -> Machine {
        Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            vec![0; BIOS_ROM_SIZE],
        )
        .unwrap()
    }

    fn memory_test_machine() -> Machine {
        Machine::new(
            MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega),
            vec![0; BIOS_ROM_SIZE],
        )
        .unwrap()
    }

    fn rom_with_code(code: &[u8]) -> Vec<u8> {
        let mut rom = vec![0; BIOS_ROM_SIZE];
        rom[..code.len()].copy_from_slice(code);
        rom[0xf000] = 0xcf;
        rom[0xfff0..0xfff5].copy_from_slice(&[0xea, 0x00, 0x00, 0x00, 0xf0]);
        rom
    }

    fn capture_error(machine: &Machine) -> MachineCanonicalCaptureError {
        machine.canonical_state_capture().err().unwrap()
    }

    fn machine_control_timing_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        machine_control_timing_payload_from_capture(&capture)
    }

    fn machine_control_timing_payload_from_capture(
        capture: &CanonicalMachineStateCapture<'_>,
    ) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0001).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_machine_control_timing_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn modeled_cache_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0002).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_modeled_cache_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn ram_rom_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0003).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_ram_rom_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn pic_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        pic_payload_from_capture(&capture)
    }

    fn pic_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0004).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_pic_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn pit_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        pit_payload_from_capture(&capture)
    }

    fn pit_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0005).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_pit_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn dma_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        dma_payload_from_capture(&capture)
    }

    fn dma_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0006).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_dma_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn dma_event_totals_v1_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        dma_event_totals_v1_payload_from_capture(&capture)
    }

    fn dma_event_totals_v1_payload_from_capture(
        capture: &CanonicalMachineStateCapture<'_>,
    ) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(TEST_DMA_EVENT_TOTALS_ENVELOPE_ID).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_dma_event_totals_v1_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn rtc_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        rtc_payload_from_capture(&capture)
    }

    fn rtc_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0007).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_rtc_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn unit_tester_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        unit_tester_payload_from_capture(&capture)
    }

    fn unit_tester_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0008).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_unit_tester_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn speaker_payload(machine: &Machine) -> Vec<u8> {
        let capture = machine.canonical_state_capture().unwrap();
        speaker_payload_from_capture(&capture)
    }

    fn speaker_payload_from_capture(capture: &CanonicalMachineStateCapture<'_>) -> Vec<u8> {
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0009).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| capture.write_speaker_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();
        view.sections()[0].payload().to_vec()
    }

    fn write_speaker_port(machine: &mut Machine, value: u8) {
        let mut bus = machine.make_bus();
        bus.write_io(0x61, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    }

    fn read_speaker_port(machine: &mut Machine) -> u8 {
        let mut bus = machine.make_bus();
        u8::try_from(bus.read_io(0x61, BusWidth::Byte, 0, false).unwrap()).unwrap()
    }

    fn write_pit_port(machine: &mut Machine, port: u16, value: u8) {
        let mut bus = machine.make_bus();
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    }

    fn read_pit_port(machine: &mut Machine, port: u16) -> u8 {
        let mut bus = machine.make_bus();
        u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
    }

    fn write_pic_port(machine: &mut Machine, port: u16, value: u8) {
        let mut bus = machine.make_bus();
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    }

    fn read_pic_port(machine: &mut Machine, port: u16) -> u8 {
        let mut bus = machine.make_bus();
        u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
    }

    fn write_rtc_port(machine: &mut Machine, port: u16, value: u8) {
        let mut bus = machine.make_bus();
        bus.write_io(port, BusWidth::Byte, u32::from(value), false)
            .unwrap();
    }

    fn read_rtc_port(machine: &mut Machine, port: u16) -> u8 {
        let mut bus = machine.make_bus();
        u8::try_from(bus.read_io(port, BusWidth::Byte, 0, false).unwrap()).unwrap()
    }

    fn initialize_pic_pair(machine: &mut Machine, level_triggered: bool) {
        let icw1 = if level_triggered { 0x19 } else { 0x11 };
        for (command, data, vector, cascade) in [(0x20, 0x21, 0x20, 0x04), (0xa0, 0xa1, 0x70, 0x02)]
        {
            write_pic_port(machine, command, icw1);
            write_pic_port(machine, data, vector);
            write_pic_port(machine, data, cascade);
            write_pic_port(machine, data, 0x01);
        }
    }

    fn warm_modeled_cache_line(machine: &mut Machine, mode: GswMode, line: u32) {
        let _ = machine.cache_model.data_tier(mode, line * CACHE_LINE_BYTES);
    }

    fn raw_word_read_clocks(machine: &mut Machine, address: u32) -> (u16, u64) {
        let before = machine.trace.elapsed_clocks();
        let value = machine.read_physical_u16(address);
        (value, machine.trace.elapsed_clocks() - before)
    }

    fn approximate_cpu_bus_cost_contract(machine: &mut Machine) -> Vec<Option<u64>> {
        let bus = machine.make_bus();
        let mut values = Vec::new();
        for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
            for kind in [BusAccessKind::DataRead, BusAccessKind::DataWrite] {
                values.push(bus.jit_direct_memory_max_clocks(width, kind));
            }
        }
        values.push(bus.jit_cached_fetch_run_clocks(0x0002_0000, 16));
        values.push(bus.jit_projected_batch_scaled_bus_clocks(37));
        values.push(Some(bus.jit_fetch_cost_clocks()));
        values.push(Some(u64::from(u8::from(bus.native_fetches_are_uniform()))));
        values.push(Some(u64::from(u8::from(
            bus.native_aggregate_accounting_allowed(),
        ))));
        values.push(Some(bus.jit_data_byte_cost_clocks()));
        for width in [BusWidth::Byte, BusWidth::Word, BusWidth::Dword] {
            values.push(Some(bus.jit_data_cost_clocks(width)));
            values.push(Some(bus.jit_mode13_data_cost_clocks(width)));
        }
        values.push(Some(bus.jit_scale_bus_cost_upper(41)));
        values.push(Some(bus.rep_data_byte_cost_upper()));
        values.push(bus.rep_page_walk_cost_upper());
        values
    }

    fn direct_charge_delta(machine: &mut Machine) -> u64 {
        let before = machine.trace.elapsed_clocks();
        {
            let mut bus = machine.make_bus();
            bus.charge_direct_memory(0x0002_0000, BusWidth::Dword, BusAccessKind::DataRead)
                .unwrap();
        }
        machine.trace.elapsed_clocks() - before
    }

    fn direct_bulk_read(machine: &mut Machine) -> (usize, [u8; 16], u64) {
        let before = machine.trace.elapsed_clocks();
        let mut bytes = [0; 16];
        let read = {
            let mut bus = machine.make_bus();
            bus.read_memory_bytes_direct(
                0x0002_0000,
                &mut bytes,
                BusWidth::Dword,
                BusAccessKind::DataRead,
            )
            .unwrap()
        };
        (read, bytes, machine.trace.elapsed_clocks() - before)
    }

    fn native_fetch_charge_delta(machine: &mut Machine) -> (bool, u64) {
        let before = machine.trace.elapsed_clocks();
        let charged = {
            let mut bus = machine.make_bus();
            bus.charge_native_cached_fetches(0x0002_0000, 0x0002_0000, &[1, 2, 3], 4)
        };
        (charged, machine.trace.elapsed_clocks() - before)
    }

    fn push_u32(bytes: &mut Vec<u8>, value: u32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(bytes: &mut Vec<u8>, value: u64) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn take_len_prefixed_bytes<'a>(payload: &'a [u8], cursor: &mut usize) -> &'a [u8] {
        let length_end = *cursor + 8;
        let length = usize::try_from(u64::from_le_bytes(
            payload[*cursor..length_end].try_into().unwrap(),
        ))
        .unwrap();
        *cursor = length_end;
        let data_end = *cursor + length;
        let data = &payload[*cursor..data_end];
        *cursor = data_end;
        data
    }

    #[test]
    fn foundation_sections_pin_ids_versions_order_and_namespaces() {
        let sections = STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS;
        assert_eq!(
            sections.map(|section| (section.id, section.version)),
            [
                (0x0000_0001, 1),
                (0x0001_0001, 1),
                (0x0001_0002, 1),
                (0x0001_0003, 1),
                (0x0002_0001, 1),
                (0x0002_0002, 1),
                (0x0002_0003, 1),
                (0x0002_0004, 1),
                (0x0002_0005, 1),
                (0x0002_0006, 1),
                (0x0002_0007, 1),
                (0x0002_0008, 1),
                (0x0002_0009, 1),
            ]
        );
        assert!(
            sections
                .iter()
                .all(|section| section.requirement == CanonicalSectionRequirement::Required)
        );
        assert!(sections.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert_eq!(
            sections[0].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_SCHEMA_NAMESPACE
        );
        assert!(sections[1..4].iter().all(|section| {
            section.id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
        }));
        assert_eq!(
            sections[4].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[5].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[6].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[7].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[8].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[9].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[10].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[11].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
        assert_eq!(
            sections[12].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK,
            STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
        );
    }

    #[test]
    fn ram_rom_projection_payload_layout_is_exact() {
        let projection = CanonicalRamRomProjection {
            ram: &[0x10, 0x20, 0x30],
            rom: &[0xa0, 0xb0],
        };
        let mut state = CanonicalStateWriter::new().unwrap();
        state
            .section(
                CanonicalSectionId::new(0x0002_0003).unwrap(),
                CanonicalSectionVersion::new(1).unwrap(),
                CanonicalSectionRequirement::Required,
                |out| projection.write_payload(out),
            )
            .unwrap();
        let bytes = state.finish().unwrap();
        let view = CanonicalStateView::parse(&bytes).unwrap();

        assert_eq!(
            view.sections()[0].payload(),
            &[
                3, 0, 0, 0, 0, 0, 0, 0, 0x10, 0x20, 0x30, 2, 0, 0, 0, 0, 0, 0, 0, 0xa0, 0xb0,
            ]
        );
    }

    #[test]
    fn ram_rom_payload_covers_both_authoritative_stores_in_raw_order() {
        let mut machine = memory_test_machine();
        let ram_len = machine.memory.len();
        for (address, value) in [
            (0x0000_0000, 0x11),
            (0x0009_ffff, 0x22),
            (0x000a_0000, 0x33),
            (0x000c_0000, 0x44),
            (0x000c_8000, 0x55),
            (0x000f_0000, 0x66),
            (0x0010_0000, 0x77),
            (ram_len - 1, 0x88),
        ] {
            machine.memory.as_mut_slice()[address] = value;
        }
        machine.rom[0] = 0x91;
        machine.rom[BIOS_ROM_SIZE / 2] = 0x92;
        machine.rom[BIOS_ROM_SIZE - 1] = 0x93;

        let payload = ram_rom_payload(&machine);
        let mut cursor = 0;
        let ram = take_len_prefixed_bytes(&payload, &mut cursor);
        let rom = take_len_prefixed_bytes(&payload, &mut cursor);

        assert_eq!(ram, machine.memory.as_slice());
        assert_eq!(rom, machine.rom.as_slice());
        assert_eq!(cursor, payload.len());
        assert_eq!(payload.len(), 16 + ram_len + BIOS_ROM_SIZE);
    }

    #[test]
    fn system_rom_aliases_share_one_store_and_ignore_guest_writes() {
        const ROM_OFFSET: usize = 0x8123;
        const ROM_VALUE: u8 = 0x5a;
        const HIDDEN_RAM_VALUE: u8 = 0xa5;

        let mut machine = memory_test_machine();
        machine.rom[ROM_OFFSET] = ROM_VALUE;
        machine.memory.as_mut_slice()[crate::LOW_BIOS_BASE as usize + ROM_OFFSET] =
            HIDDEN_RAM_VALUE;
        let expected = ram_rom_payload(&machine);

        {
            let mut bus = machine.make_bus();
            assert_eq!(
                CpuBus::read_memory(
                    &mut bus,
                    crate::LOW_BIOS_BASE + ROM_OFFSET as u32,
                    BusWidth::Byte,
                    BusAccessKind::DataRead,
                )
                .unwrap(),
                u32::from(ROM_VALUE)
            );
            assert_eq!(
                CpuBus::read_memory(
                    &mut bus,
                    crate::HIGH_ROM_BASE + ROM_OFFSET as u32,
                    BusWidth::Byte,
                    BusAccessKind::DataRead,
                )
                .unwrap(),
                u32::from(ROM_VALUE)
            );
            CpuBus::write_memory(
                &mut bus,
                crate::LOW_BIOS_BASE + ROM_OFFSET as u32,
                BusWidth::Byte,
                0x10,
                BusAccessKind::DataWrite,
            )
            .unwrap();
            CpuBus::write_memory(
                &mut bus,
                crate::HIGH_ROM_BASE + ROM_OFFSET as u32,
                BusWidth::Byte,
                0x20,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }

        assert_eq!(machine.rom[ROM_OFFSET], ROM_VALUE);
        assert_eq!(
            machine.memory.as_slice()[crate::LOW_BIOS_BASE as usize + ROM_OFFSET],
            HIDDEN_RAM_VALUE
        );
        assert_eq!(ram_rom_payload(&machine), expected);
    }

    #[test]
    fn discarded_flash_prefix_is_not_canonical_state() {
        let mut first = vec![0; BIOS_ROM_SIZE * 2];
        let mut second = first.clone();
        first[0] = 0x11;
        second[0] = 0x22;
        let profile = MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega);
        let first = Machine::new(profile.clone(), first).unwrap();
        let second = Machine::new(profile, second).unwrap();

        assert_eq!(first.rom, second.rom);
        assert_eq!(ram_rom_payload(&first), ram_rom_payload(&second));
    }

    #[test]
    fn a20_routing_changes_raw_cells_without_projecting_an_alias() {
        const LOW: usize = 0x0000_1234;
        const HIGH: u32 = 0x0010_1234;

        let mut machine = memory_test_machine();
        let unchanged = ram_rom_payload(&machine);
        {
            let mut bus = machine.make_bus();
            bus.write_io(0x0092, BusWidth::Byte, 0, false).unwrap();
        }
        assert_eq!(ram_rom_payload(&machine), unchanged);

        {
            let mut bus = machine.make_bus();
            CpuBus::write_memory(
                &mut bus,
                HIGH,
                BusWidth::Byte,
                0x41,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }
        assert_eq!(machine.memory.as_slice()[LOW], 0x41);
        assert_eq!(machine.memory.as_slice()[HIGH as usize], 0);

        {
            let mut bus = machine.make_bus();
            bus.write_io(0x0092, BusWidth::Byte, 0x02, false).unwrap();
            CpuBus::write_memory(
                &mut bus,
                HIGH,
                BusWidth::Byte,
                0x52,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }
        assert_eq!(machine.memory.as_slice()[LOW], 0x41);
        assert_eq!(machine.memory.as_slice()[HIGH as usize], 0x52);

        let payload = ram_rom_payload(&machine);
        let mut cursor = 0;
        let ram = take_len_prefixed_bytes(&payload, &mut cursor);
        assert_eq!(ram[LOW], 0x41);
        assert_eq!(ram[HIGH as usize], 0x52);
    }

    #[test]
    fn live_pci_decode_rebuilds_and_publishes_the_excluded_ram_lookup() {
        const RAM_BAR: u32 = 0x0100_0000;
        const BAR_REGISTER: u32 = 0x10;
        const COMMAND_REGISTER: u32 = 0x04;

        let mut machine = Machine::new(
            MachineProfile::gsw_386(32, VideoCard::Vega),
            rom_with_code(&[0xf4]),
        )
        .unwrap();
        let expected = ram_rom_payload(&machine);
        assert!(
            machine
                .ram_lookup
                .is_consistent(machine.memory.len(), &machine.vega)
        );

        {
            let mut bus = machine.make_bus();
            assert!(
                CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                    .unwrap()
                    .is_some()
            );
            let config_address =
                0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | COMMAND_REGISTER;
            bus.write_io(
                crate::PCI_CONFIG_ADDRESS_PORT,
                BusWidth::Dword,
                config_address,
                false,
            )
            .unwrap();
            bus.write_io(crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword, 0, false)
                .unwrap();
            let config_address =
                0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | BAR_REGISTER;
            bus.write_io(
                crate::PCI_CONFIG_ADDRESS_PORT,
                BusWidth::Dword,
                config_address,
                false,
            )
            .unwrap();
            bus.write_io(crate::PCI_CONFIG_DATA_PORT, BusWidth::Dword, RAM_BAR, false)
                .unwrap();
            assert!(
                CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                    .unwrap()
                    .is_some()
            );
            let config_address =
                0x8000_0000 | (u32::from(crate::DISTIRA_PCI_SLOT) << 11) | COMMAND_REGISTER;
            bus.write_io(
                crate::PCI_CONFIG_ADDRESS_PORT,
                BusWidth::Dword,
                config_address,
                false,
            )
            .unwrap();
            bus.write_io(
                crate::PCI_CONFIG_DATA_PORT,
                BusWidth::Dword,
                0x0000_0002,
                false,
            )
            .unwrap();
            assert!(
                CpuBus::direct_page(&mut bus, RAM_BAR, BusAccessKind::DataRead)
                    .unwrap()
                    .is_none()
            );
        }

        assert_eq!(
            capture_error(&machine),
            MachineCanonicalCaptureError::PendingDirectMapChange
        );
        assert!(
            machine
                .ram_lookup
                .is_consistent(machine.memory.len(), &machine.vega)
        );

        assert_eq!(
            machine.run_until_halt_or_cycles(100_000).unwrap(),
            StopReason::Halted
        );
        assert!(!machine.direct_map_changed);
        assert!(
            machine
                .ram_lookup
                .is_consistent(machine.memory.len(), &machine.vega)
        );
        assert_eq!(ram_rom_payload(&machine), expected);
    }

    #[test]
    fn fresh_modeled_cache_payload_is_exact_in_every_mode() {
        for mode in [
            GswMode::Gsw386Slow,
            GswMode::Gsw386,
            GswMode::Gsw486,
            GswMode::Gsw586,
        ] {
            let mut machine = test_machine();
            machine.set_mode(mode);

            assert_eq!(
                modeled_cache_payload(&machine),
                vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn populated_modeled_cache_payload_sorts_full_tags_numerically() {
        let mut expected = Vec::new();
        push_u64(&mut expected, 0);
        push_u64(&mut expected, 3);
        push_u32(&mut expected, 0x0000_0002);
        push_u32(&mut expected, 0x0000_0401);
        push_u32(&mut expected, 0x0000_0800);
        assert_eq!(expected.len(), 28);

        for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
            let mut forward = test_machine();
            forward.set_mode(mode);
            for line in [0x0800, 0x0002, 0x0401] {
                warm_modeled_cache_line(&mut forward, mode, line);
            }

            let mut reverse = test_machine();
            reverse.set_mode(mode);
            for line in [0x0401, 0x0002, 0x0800] {
                warm_modeled_cache_line(&mut reverse, mode, line);
            }

            assert_eq!(modeled_cache_payload(&forward), expected, "{mode:?}");
            assert_eq!(modeled_cache_payload(&reverse), expected, "{mode:?}");
        }
    }

    #[test]
    fn accurate_cache_hit_and_collision_preserve_next_access_timing() {
        const TARGET_LINE: u32 = 0x0500;
        const COLLIDING_LINE: u32 = TARGET_LINE + 0x0400;
        const TARGET_ADDRESS: u32 = TARGET_LINE * CACHE_LINE_BYTES;

        for mode in [GswMode::Gsw386Slow, GswMode::Gsw386] {
            let mut hot = test_machine();
            hot.set_mode(mode);
            warm_modeled_cache_line(&mut hot, mode, TARGET_LINE);

            let mut displaced = test_machine();
            displaced.set_mode(mode);
            warm_modeled_cache_line(&mut displaced, mode, COLLIDING_LINE);

            assert_eq!(hot.cache_tier_lookups(), displaced.cache_tier_lookups());
            assert_ne!(
                modeled_cache_payload(&hot),
                modeled_cache_payload(&displaced),
                "{mode:?}"
            );

            let hot_read = raw_word_read_clocks(&mut hot, TARGET_ADDRESS);
            let displaced_read = raw_word_read_clocks(&mut displaced, TARGET_ADDRESS);
            assert_eq!(hot_read.0, displaced_read.0, "{mode:?}");
            assert_eq!(hot_read.1, 2, "{mode:?} L2 hit");
            assert_eq!(displaced_read.1, 5, "{mode:?} RAM miss");
            assert_eq!(
                modeled_cache_payload(&hot),
                modeled_cache_payload(&displaced),
                "{mode:?} caches must converge after the same access"
            );
        }
    }

    #[test]
    fn inert_modeled_cache_residue_is_payload_and_continuation_neutral() {
        let mut clean = test_machine();
        let mut residue = test_machine();
        clean.set_mode(GswMode::Gsw386);
        residue.set_mode(GswMode::Gsw386);

        residue.cache_model.l1_tags[0] = 0;
        residue.cache_model.l2_tags[3] = 2;
        residue.cache_model.l2_tags[0] = MAX_MODELED_CACHE_LINE + 1;
        residue.cache_model.l2_tags[1024] = 0x0400;
        residue.cache_model.l2_tags[4] = crate::CACHE_EMPTY_TAG;

        assert_eq!(
            modeled_cache_payload(&clean),
            modeled_cache_payload(&residue)
        );

        for address in [2 * CACHE_LINE_BYTES, 0] {
            let clean_read = raw_word_read_clocks(&mut clean, address);
            let residue_read = raw_word_read_clocks(&mut residue, address);
            assert_eq!(clean_read, residue_read, "address {address:#010x}");
        }
        assert_eq!(
            modeled_cache_payload(&clean),
            modeled_cache_payload(&residue)
        );
    }

    #[test]
    fn modeled_cache_capture_is_read_only_and_excludes_lookup_count() {
        const LINE: u32 = 0x0123;
        let mut once = test_machine();
        let mut repeated = test_machine();
        warm_modeled_cache_line(&mut once, GswMode::Gsw386, LINE);
        for _ in 0..4 {
            warm_modeled_cache_line(&mut repeated, GswMode::Gsw386, LINE);
        }
        assert_ne!(once.cache_tier_lookups(), repeated.cache_tier_lookups());
        assert_eq!(
            modeled_cache_payload(&once),
            modeled_cache_payload(&repeated)
        );

        let before_l1 = repeated.cache_model.l1_tags.to_vec();
        let before_l2 = repeated.cache_model.l2_tags.to_vec();
        let before_config = (
            repeated.cache_model.config.l1_mask,
            repeated.cache_model.config.l2_mask,
        );
        let before_cost = (
            repeated.cache_model.cost.l1,
            repeated.cache_model.cost.l2,
            repeated.cache_model.cost.ram,
        );
        let before_code_fetch_ws = repeated.cache_model.code_fetch_ws;
        let before_lookups = repeated.cache_tier_lookups();
        let first = modeled_cache_payload(&repeated);
        let second = modeled_cache_payload(&repeated);

        assert_eq!(first, second);
        assert_eq!(repeated.cache_model.l1_tags.as_ref(), before_l1);
        assert_eq!(repeated.cache_model.l2_tags.as_ref(), before_l2);
        assert_eq!(
            (
                repeated.cache_model.config.l1_mask,
                repeated.cache_model.config.l2_mask,
            ),
            before_config
        );
        assert_eq!(
            (
                repeated.cache_model.cost.l1,
                repeated.cache_model.cost.l2,
                repeated.cache_model.cost.ram,
            ),
            before_cost
        );
        assert_eq!(repeated.cache_model.code_fetch_ws, before_code_fetch_ws);
        assert_eq!(repeated.cache_tier_lookups(), before_lookups);

        let once_read = raw_word_read_clocks(&mut once, LINE * CACHE_LINE_BYTES);
        let repeated_read = raw_word_read_clocks(&mut repeated, LINE * CACHE_LINE_BYTES);
        assert_eq!(once_read, repeated_read);
    }

    #[test]
    fn approximate_modes_ignore_all_tag_residue_on_normal_bus_accesses() {
        const ADDRESS: u32 = 0x0002_0000;
        for mode in [GswMode::Gsw486, GswMode::Gsw586] {
            let mut clean = test_machine();
            let mut residue = test_machine();
            clean.set_mode(mode);
            residue.set_mode(mode);

            residue.cache_model.l1_tags[0] = 0;
            residue.cache_model.l1_tags[1] = MAX_MODELED_CACHE_LINE + 1;
            residue.cache_model.l2_tags[3] = 2;
            residue.cache_model.l2_tags[CACHE_L2_MAX_LINES - 1] = 0x1fff;
            warm_modeled_cache_line(&mut residue, mode, ADDRESS / CACHE_LINE_BYTES);
            let lookups = residue.cache_tier_lookups();
            let l1_tags = residue.cache_model.l1_tags.to_vec();
            let l2_tags = residue.cache_model.l2_tags.to_vec();

            assert_eq!(
                modeled_cache_payload(&clean),
                vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                "{mode:?}"
            );
            assert_eq!(
                modeled_cache_payload(&residue),
                vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                "{mode:?}"
            );

            let clean_read = raw_word_read_clocks(&mut clean, ADDRESS);
            let residue_read = raw_word_read_clocks(&mut residue, ADDRESS);
            assert_eq!(clean_read, residue_read, "{mode:?} read");
            assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?} read");

            let clean_before = clean.trace.elapsed_clocks();
            let residue_before = residue.trace.elapsed_clocks();
            clean.write_physical_u16(ADDRESS, 0x5aa5);
            residue.write_physical_u16(ADDRESS, 0x5aa5);
            assert_eq!(
                clean.trace.elapsed_clocks() - clean_before,
                residue.trace.elapsed_clocks() - residue_before,
                "{mode:?} write"
            );
            assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?} write");
            assert_eq!(residue.cache_model.l1_tags.as_ref(), l1_tags, "{mode:?}");
            assert_eq!(residue.cache_model.l2_tags.as_ref(), l2_tags, "{mode:?}");
        }
    }

    #[test]
    fn approximate_direct_and_native_bus_contract_ignores_tag_residue() {
        for mode in [GswMode::Gsw486, GswMode::Gsw586] {
            let mut clean = test_machine();
            let mut residue = test_machine();
            clean.set_mode(mode);
            residue.set_mode(mode);

            residue.cache_model.l1_tags.fill(0);
            residue.cache_model.l2_tags.fill(0);
            residue.cache_model.l1_tags[1] = MAX_MODELED_CACHE_LINE + 1;
            residue.cache_model.l2_tags[3] = 2;
            warm_modeled_cache_line(&mut residue, mode, 0x0800);
            let lookups = residue.cache_tier_lookups();
            let l1_tags = residue.cache_model.l1_tags.to_vec();
            let l2_tags = residue.cache_model.l2_tags.to_vec();

            assert_eq!(
                approximate_cpu_bus_cost_contract(&mut clean),
                approximate_cpu_bus_cost_contract(&mut residue),
                "{mode:?} cost contract"
            );
            assert_eq!(
                direct_charge_delta(&mut clean),
                direct_charge_delta(&mut residue),
                "{mode:?} direct charge"
            );
            assert_eq!(
                direct_bulk_read(&mut clean),
                direct_bulk_read(&mut residue),
                "{mode:?} direct bulk read"
            );
            assert_eq!(
                native_fetch_charge_delta(&mut clean),
                native_fetch_charge_delta(&mut residue),
                "{mode:?} native fetch charge"
            );

            assert_eq!(residue.cache_tier_lookups(), lookups, "{mode:?}");
            assert_eq!(residue.cache_model.l1_tags.as_ref(), l1_tags, "{mode:?}");
            assert_eq!(residue.cache_model.l2_tags.as_ref(), l2_tags, "{mode:?}");
            assert_eq!(
                modeled_cache_payload(&residue),
                vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn effective_tags_do_not_depend_on_current_a20_or_device_decode() {
        const DEVICE_LINE: u32 = 0x000a_0000 / CACHE_LINE_BYTES;
        const HIGH_LINE: u32 = 0x0010_0040 / CACHE_LINE_BYTES;
        let mut machine = test_machine();
        warm_modeled_cache_line(&mut machine, GswMode::Gsw386, DEVICE_LINE);
        warm_modeled_cache_line(&mut machine, GswMode::Gsw386, HIGH_LINE);
        let expected = modeled_cache_payload(&machine);

        machine.set_a20_gate(false);

        assert_eq!(modeled_cache_payload(&machine), expected);
        assert!(
            expected
                .windows(4)
                .any(|bytes| bytes == DEVICE_LINE.to_le_bytes())
        );
        assert!(
            expected
                .windows(4)
                .any(|bytes| bytes == HIGH_LINE.to_le_bytes())
        );
    }

    #[test]
    fn every_mode_change_resets_raw_and_effective_cache_state() {
        let modes = [
            GswMode::Gsw386Slow,
            GswMode::Gsw386,
            GswMode::Gsw486,
            GswMode::Gsw586,
        ];
        for source in modes {
            for target in modes {
                let mut machine = test_machine();
                machine.set_mode(source);
                machine.cache_model.l1_tags.fill(0);
                machine.cache_model.l2_tags.fill(0);
                warm_modeled_cache_line(&mut machine, source, 0x0123);
                let lookups = machine.cache_tier_lookups();

                machine.set_mode(target);

                assert!(
                    machine
                        .cache_model
                        .l1_tags
                        .iter()
                        .all(|tag| *tag == crate::CACHE_EMPTY_TAG),
                    "{source:?} -> {target:?} L1"
                );
                assert!(
                    machine
                        .cache_model
                        .l2_tags
                        .iter()
                        .all(|tag| *tag == crate::CACHE_EMPTY_TAG),
                    "{source:?} -> {target:?} L2"
                );
                assert_eq!(machine.cache_tier_lookups(), lookups);
                assert_eq!(
                    modeled_cache_payload(&machine),
                    vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN],
                    "{source:?} -> {target:?}"
                );
                if !target.uses_approximate_timing() {
                    assert_eq!(
                        raw_word_read_clocks(&mut machine, 0x0123 * CACHE_LINE_BYTES).1,
                        5,
                        "{source:?} -> {target:?} must resume cold"
                    );
                }
            }
        }
    }

    #[test]
    fn default_machine_control_timing_payload_is_exactly_pinned() {
        let payload = machine_control_timing_payload(&test_machine());
        let mut expected = vec![
            0x10, 0x00, // memory MiB
            0x00, 0x01, 0x01, 0x02, // RAM, ROM, video, and I/O wait states
            0x01, // fast POST
            0x00, 0x00, // no effective pending software INT
            0x00, 0x00, // no intercepted INT stash
        ];
        expected.resize(MACHINE_CONTROL_TIMING_PAYLOAD_LEN, 0);

        assert_eq!(payload, expected);
    }

    #[test]
    fn populated_machine_control_timing_payload_pins_every_field_offset() {
        let mut profile = MachineProfile::gsw_386(24, VideoCard::Vega);
        profile.wait_states = WaitStateProfile {
            ram: 3,
            rom: 5,
            video: 7,
            io: 11,
        };
        let mut machine = Machine::new(profile, vec![0; BIOS_ROM_SIZE]).unwrap();
        machine.set_mode(GswMode::Gsw586);
        machine.cpu.control.cr0 |= 1;
        machine.set_fast_post(false);
        machine.pending_soft_int = Some(0x21);
        machine.last_int_vector = Some(0x13);
        machine.timeline.advance_io_stall_ticks(
            34,
            crate::timeline::DeviceRates {
                dsp_hz: 2,
                wss_hz: 3,
                cd_playing: true,
                vga_dot_hz: 4,
            },
        );
        machine.elapsed_clocks = 200;
        machine.io_stall_clocks = 40;
        machine.halted_ticks = 20;
        machine.trace.add_elapsed_clocks(300);
        machine.scaled_bus_clocks = 150;
        machine.bus_rem = 29;

        let payload = machine_control_timing_payload(&machine);
        let mut expected = vec![
            0x18, 0x00, // memory MiB
            3, 5, 7, 11, // wait states
            0,  // full POST pacing
            1, 0x21, // pending software INT
            1, 0x13, // intercepted INT stash
        ];
        for value in [
            34,
            34,
            1,
            34_000_000,
            40_568_188,
            68,
            102,
            2_550,
            34,
            1_000_000_000,
            2_040,
            1_071_000,
            136,
            200,
            40,
            20,
            300,
            150,
            29,
        ] {
            push_u64(&mut expected, value);
        }

        assert_eq!(payload.len(), MACHINE_CONTROL_TIMING_PAYLOAD_LEN);
        assert_eq!(payload, expected);
    }

    #[test]
    fn clean_capture_is_read_only_and_accepts_ring_zero_state() {
        let mut machine = test_machine();
        machine.cpu.control.cr0 |= 1;
        assert!(machine.cpu.is_ring0_protected());
        let before_cpu = machine.cpu.clone();
        let before_timeline = machine.timeline;
        let before_clocks = (
            machine.elapsed_clocks,
            machine.scaled_bus_clocks,
            machine.trace.elapsed_clocks(),
        );

        let first_payload = machine_control_timing_payload(&machine);
        let second_payload = machine_control_timing_payload(&machine);

        assert_eq!(machine.cpu, before_cpu);
        assert_eq!(machine.timeline, before_timeline);
        assert_eq!(first_payload, second_payload);
        assert_eq!(
            (
                machine.elapsed_clocks,
                machine.scaled_bus_clocks,
                machine.trace.elapsed_clocks(),
            ),
            before_clocks
        );
    }

    #[test]
    fn dma_semantic_state_and_event_totals_share_one_read_only_capture() {
        let mut machine = test_machine();
        machine.dma.write_port(0x0c, 0);
        machine.dma.write_port(0x00, 0x34);
        machine.dma.master.channels[0].transfer_cycles = 17;
        machine.dma.master.channels[2].transfer_cycles = 29;

        let capture = machine.canonical_state_capture().unwrap();
        let first_state = dma_payload_from_capture(&capture);
        let first_totals = dma_event_totals_v1_payload_from_capture(&capture);
        let second_totals = dma_event_totals_v1_payload_from_capture(&capture);
        let second_state = dma_payload_from_capture(&capture);

        assert_eq!(first_state.len(), DMA_PAYLOAD_LEN);
        assert_eq!(first_totals.len(), DMA_EVENT_TOTALS_V1_PAYLOAD_LEN);
        assert_eq!(first_state, second_state);
        assert_eq!(first_totals, second_totals);
        assert_eq!(first_state[0], 1, "half-write continuation is retained");
        assert_eq!(
            u64::from_le_bytes(first_totals[0..8].try_into().unwrap()),
            17
        );
        assert_eq!(
            u64::from_le_bytes(first_totals[16..24].try_into().unwrap()),
            29
        );
        drop(capture);

        machine.dma.write_port(0x00, 0x12);
        assert_eq!(machine.dma.master.channels[0].base_addr, 0x1234);
        assert_eq!(dma_event_totals_v1_payload(&machine), first_totals);
        assert_ne!(dma_payload(&machine), first_state);
    }

    #[test]
    fn rtc_pic_and_timeline_payloads_share_one_read_only_capture() {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        machine.seed_rtc(2026, 7, 19, 1, 23, 58, 41);
        initialize_pic_pair(&mut machine, false);
        write_rtc_port(&mut machine, 0x70, 0x0b);
        write_rtc_port(&mut machine, 0x71, 0x40);
        let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();
        machine.advance_devices_ticks(deadline / 3);

        let capture = machine.canonical_state_capture().unwrap();
        let first_rtc = rtc_payload_from_capture(&capture);
        let first_pic = pic_payload_from_capture(&capture);
        let first_timeline = machine_control_timing_payload_from_capture(&capture);
        let second_timeline = machine_control_timing_payload_from_capture(&capture);
        let second_pic = pic_payload_from_capture(&capture);
        let second_rtc = rtc_payload_from_capture(&capture);

        assert_eq!(first_rtc.len(), RTC_PAYLOAD_LEN);
        assert_eq!(first_rtc, second_rtc);
        assert_eq!(first_pic, second_pic);
        assert_eq!(first_timeline, second_timeline);
        assert_eq!(
            machine.rtc.ticks_until_periodic_irq(),
            Some(deadline - deadline / 3)
        );
    }

    #[test]
    fn guest_cmos_dirty_notification_is_captureable_and_normalized() {
        let mut machine = test_machine();
        machine.rtc.write_port(0x70, 0x10);
        let value = machine.cmos_byte(0x10);
        let clean = rtc_payload(&machine);

        machine.rtc.write_port(0x71, value);
        let capture = machine.canonical_state_capture().unwrap();
        assert_eq!(rtc_payload_from_capture(&capture), clean);
        drop(capture);

        assert!(machine.take_cmos_dirty());
        assert_eq!(rtc_payload(&machine), clean);
        assert!(!machine.take_cmos_dirty());
    }

    #[test]
    fn serviced_unit_tester_crc_changes_only_the_result_registers() {
        let program_rectangle = [
            0xb0, 0x00, 0xe6, 0xe4, // select rectangle register zero
            0xb0, 0x00, 0xe6, 0xe5, // X low
            0xb0, 0x00, 0xe6, 0xe5, // X high
            0xb0, 0x00, 0xe6, 0xe5, // Y low
            0xb0, 0x00, 0xe6, 0xe5, // Y high
            0xb0, 0x02, 0xe6, 0xe5, // W low
            0xb0, 0x00, 0xe6, 0xe5, // W high
            0xb0, 0x02, 0xe6, 0xe5, // H low
            0xb0, 0x00, 0xe6, 0xe5, // H high
        ];
        let mut baseline_code = program_rectangle.to_vec();
        baseline_code.push(0xf4);
        let mut serviced_code = program_rectangle.to_vec();
        serviced_code.extend_from_slice(&[
            0xb0,
            unittester::CMD_CRC,
            0xe6,
            0xe6, // issue deferred CRC command
            0xf4,
        ]);
        let mut baseline = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            rom_with_code(&baseline_code),
        )
        .unwrap();
        let mut serviced = Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Vega),
            rom_with_code(&serviced_code),
        )
        .unwrap();

        assert_eq!(
            baseline.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(
            serviced.run_until_halt_or_cycles(1_000_000).unwrap(),
            StopReason::Halted
        );
        assert_eq!(serviced.unittester.pending_command(), None);

        let before = unit_tester_payload(&baseline);
        let after = unit_tester_payload(&serviced);
        let crc = serviced.screen_crc32(0, 0, 2, 2).to_le_bytes();
        let mut expected = before.clone();
        expected[1 + unittester::REG_CRC..1 + unittester::REG_CRC + 4].copy_from_slice(&crc);

        assert_eq!(before[0], 8);
        assert_eq!(after[0], 8);
        assert_eq!(after, expected);
        assert_ne!(crc, [0; 4]);
    }

    #[test]
    fn serviced_unit_tester_noop_commands_leave_the_payload_stable() {
        let expected = unit_tester_payload(&test_machine());
        for command in [unittester::CMD_SNAPSHOT, 0x7f] {
            let rom = rom_with_code(&[
                0xb0, command, 0xe6, 0xe6, // issue deferred command
                0xf4,
            ]);
            let mut machine =
                Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

            assert_eq!(
                machine.run_until_halt_or_cycles(1_000_000).unwrap(),
                StopReason::Halted
            );
            assert_eq!(machine.unittester.pending_command(), None);
            assert_eq!(unit_tester_payload(&machine), expected);
        }
    }

    #[test]
    fn speaker_payload_pins_every_port_61_latch_value() {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);

        for value in u8::MIN..=u8::MAX {
            write_speaker_port(&mut machine, value);
            let expected = value & 0x03;

            assert_eq!(speaker_payload(&machine), [expected]);
            assert_eq!(read_speaker_port(&mut machine) & 0x03, expected);
            assert_eq!(
                pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET],
                u8::from(expected & 1 != 0)
            );
        }
    }

    #[test]
    fn speaker_latch_and_pit_gate_remain_independent_owners() {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);

        write_speaker_port(&mut machine, 0x00);
        machine.set_timer_gate(2, true);
        assert_eq!(speaker_payload(&machine), [0]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 1);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 0);

        write_speaker_port(&mut machine, 0x01);
        machine.set_timer_gate(2, false);
        assert_eq!(speaker_payload(&machine), [1]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 0);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 1);
    }

    #[test]
    fn pit_advancement_changes_pit_without_changing_the_speaker_latch() {
        let mut machine = test_machine();
        write_pit_port(&mut machine, 0x43, 0xb6);
        write_pit_port(&mut machine, 0x42, 4);
        write_pit_port(&mut machine, 0x42, 0);
        write_speaker_port(&mut machine, 0x03);
        let speaker_before = speaker_payload(&machine);
        let pit_before = pit_payload(&machine);

        let pit_ticks = MASTER_CLOCK_HZ / u64::from(crate::PIT_INPUT_HZ) * 16;
        machine.advance_devices_ticks(pit_ticks);

        assert_ne!(pit_payload(&machine), pit_before);
        assert_eq!(speaker_payload(&machine), speaker_before);
    }

    #[test]
    fn speaker_capture_is_read_only_and_excludes_host_audio_history() {
        let mut machine = test_machine();
        write_speaker_port(&mut machine, 0x03);
        machine
            .speaker
            .accumulate(MASTER_CLOCK_HZ / 100, true, [(17, false), (31, true)]);
        machine.speaker_transitions.push(crate::pit::OutTransition {
            tick: 7,
            level: false,
        });
        let mut expected_speaker = machine.speaker.clone();
        let transitions_before = machine.speaker_transitions.clone();
        let pit_before = machine.pit.clone();
        let timeline_before = machine.timeline;

        let capture = machine.canonical_state_capture().unwrap();
        let first_speaker = speaker_payload_from_capture(&capture);
        let captured_pit = pit_payload_from_capture(&capture);
        let second_speaker = speaker_payload_from_capture(&capture);
        drop(capture);

        assert_eq!(first_speaker, [3]);
        assert_eq!(first_speaker, second_speaker);
        assert_eq!(captured_pit, pit_payload(&machine));
        assert_eq!(machine.speaker_transitions, transitions_before);
        assert_eq!(machine.pit, pit_before);
        assert_eq!(machine.timeline, timeline_before);
        assert_eq!(machine.speaker.drain(64), expected_speaker.drain(64));

        let mut baseline = test_machine();
        let mut history = test_machine();
        history.speaker.write_control(0x03);
        history
            .speaker
            .accumulate(MASTER_CLOCK_HZ / 50, true, [(23, false), (71, true)]);
        let _ = history.speaker.drain(11);
        history.speaker_transitions.push(crate::pit::OutTransition {
            tick: 5,
            level: true,
        });
        history.set_speaker_volume(0.25);
        history.speaker.write_control(0x00);
        baseline.speaker.write_control(0x00);

        assert!(history.speaker.ever_enabled());
        assert!(!history.speaker_transitions.is_empty());
        assert_ne!(history.speaker_volume, baseline.speaker_volume);
        assert_eq!(speaker_payload(&history), speaker_payload(&baseline));
    }

    #[test]
    fn rtc_periodic_update_and_alarm_state_is_batch_invariant() {
        let mut whole = test_machine();
        let mut split = test_machine();
        for machine in [&mut whole, &mut split] {
            machine.set_mode(GswMode::Gsw586);
            machine.seed_rtc(2026, 7, 19, 1, 10, 30, 44);
            initialize_pic_pair(machine, false);
            for (index, value) in [
                (0x0a, 0x2f),
                (0x01, 45),
                (0x03, 30),
                (0x05, 10),
                (0x0b, 0x70),
            ] {
                write_rtc_port(machine, 0x70, index);
                write_rtc_port(machine, 0x71, value);
            }
        }

        whole.advance_devices_ticks(MASTER_CLOCK_HZ);
        let first_split = MASTER_CLOCK_HZ * 2 / 3;
        split.advance_devices_ticks(first_split);
        assert_eq!(rtc_payload(&split)[0x0c] & 0xf0, 0xc0);
        assert!(split.pic.irr_bit(8));
        let pic_after_periodic = pic_payload(&split);
        split.advance_devices_ticks(MASTER_CLOCK_HZ - first_split);
        assert_eq!(pic_payload(&split), pic_after_periodic);

        assert_eq!(rtc_payload(&whole), rtc_payload(&split));
        assert_eq!(pic_payload(&whole), pic_payload(&split));
        assert_eq!(
            machine_control_timing_payload(&whole),
            machine_control_timing_payload(&split)
        );
        assert_eq!(whole.rtc.clock(), split.rtc.clock());
        assert_eq!(rtc_payload(&whole)[0x0c] & 0xf0, 0xf0);
        assert!(whole.pic.irr_bit(8));
        assert!(split.pic.irr_bit(8));
    }

    #[test]
    fn rtc_and_pic_commit_the_irq8_deadline_at_one_capture_boundary() {
        let mut machine = test_machine();
        machine.set_mode(GswMode::Gsw586);
        initialize_pic_pair(&mut machine, false);
        write_rtc_port(&mut machine, 0x70, 0x0b);
        write_rtc_port(&mut machine, 0x71, 0x40);
        let deadline = machine.rtc.ticks_until_periodic_irq().unwrap();

        machine.advance_devices_ticks(deadline - 1);
        let before = machine.canonical_state_capture().unwrap();
        assert_eq!(rtc_payload_from_capture(&before)[0x0c] & 0xc0, 0);
        assert_eq!(pic_payload_from_capture(&before)[17] & 0x01, 0);
        drop(before);

        machine.advance_devices_ticks(1);
        let at_edge = machine.canonical_state_capture().unwrap();
        assert_eq!(rtc_payload_from_capture(&at_edge)[0x0c] & 0xc0, 0xc0);
        assert_eq!(pic_payload_from_capture(&at_edge)[17] & 0x01, 0x01);
        drop(at_edge);

        write_rtc_port(&mut machine, 0x70, 0x0c);
        assert_eq!(read_rtc_port(&mut machine, 0x71) & 0xc0, 0xc0);
        assert_eq!(rtc_payload(&machine)[0x0c], 0);
        assert!(machine.pic.irr_bit(8));
        assert_eq!(pic_payload(&machine)[17] & 0x01, 0x01);
    }

    #[test]
    fn batch_scratch_and_pending_semantic_state_remain_captureable() {
        let mut machine = test_machine();
        machine.io_touched = true;
        machine.pending_soft_int = Some(0x21);
        machine.pic.request(5);

        let capture = machine.canonical_state_capture().unwrap();
        drop(capture);
        let payload = pic_payload(&machine);

        assert!(machine.io_touched);
        assert_eq!(machine.pending_soft_int, Some(0x21));
        assert!(machine.pic.irr_bit(5));
        assert_eq!(payload[0] & (1 << 5), 1 << 5);
        assert_eq!(pit_payload(&machine).len(), PIT_PAYLOAD_LEN);
        assert_eq!(dma_payload(&machine).len(), DMA_PAYLOAD_LEN);
        assert_eq!(rtc_payload(&machine).len(), RTC_PAYLOAD_LEN);
        assert_eq!(unit_tester_payload(&machine).len(), UNIT_TESTER_PAYLOAD_LEN);
        assert_eq!(speaker_payload(&machine).len(), SPEAKER_PAYLOAD_LEN);
        assert_eq!(
            dma_event_totals_v1_payload(&machine).len(),
            DMA_EVENT_TOTALS_V1_PAYLOAD_LEN
        );
    }

    #[test]
    fn pic_capture_preserves_an_armed_destructive_poll_read() {
        let mut machine = test_machine();
        initialize_pic_pair(&mut machine, false);
        machine.pic.request(3);
        write_pic_port(&mut machine, 0x20, 0x0c);

        let first = pic_payload(&machine);
        let second = pic_payload(&machine);

        assert_eq!(first.len(), PIC_PAYLOAD_LEN);
        assert_eq!(first, second);
        assert_eq!(first[12], 1);
        assert_eq!(read_pic_port(&mut machine, 0x20), 0x83);
        let consumed = pic_payload(&machine);
        assert_eq!(consumed[12], 0);
        assert_eq!(consumed[2] & (1 << 3), 1 << 3);
    }

    #[test]
    fn pic_payload_preserves_held_master_and_slave_level_continuation() {
        let mut machine = test_machine();
        initialize_pic_pair(&mut machine, true);

        machine.pic.set_irq_level(3, true);
        let master_pending = pic_payload(&machine);
        assert_eq!(master_pending[0] & (1 << 3), 1 << 3);
        assert_eq!(master_pending[1] & (1 << 3), 1 << 3);
        assert_eq!(machine.pic.acknowledge(), Some(0x23));
        let master_in_service = pic_payload(&machine);
        assert_eq!(master_in_service[0] & (1 << 3), 0);
        assert_eq!(master_in_service[2] & (1 << 3), 1 << 3);
        write_pic_port(&mut machine, 0x20, 0x20);
        let master_reasserted = pic_payload(&machine);
        assert_eq!(master_reasserted[0] & (1 << 3), 1 << 3);
        assert_eq!(master_reasserted[2] & (1 << 3), 0);
        machine.pic.set_irq_level(3, false);

        machine.pic.set_irq_level(10, true);
        let slave_pending = pic_payload(&machine);
        assert_eq!(slave_pending[17] & (1 << 2), 1 << 2);
        assert_eq!(slave_pending[18] & (1 << 2), 1 << 2);
        assert_eq!(machine.pic.acknowledge(), Some(0x72));
        let slave_in_service = pic_payload(&machine);
        assert_eq!(slave_in_service[17] & (1 << 2), 0);
        assert_eq!(slave_in_service[19] & (1 << 2), 1 << 2);
        write_pic_port(&mut machine, 0xa0, 0x20);
        write_pic_port(&mut machine, 0x20, 0x20);
        let slave_reasserted = pic_payload(&machine);
        assert_eq!(slave_reasserted[0] & (1 << 2), 1 << 2);
        assert_eq!(slave_reasserted[1] & (1 << 2), 1 << 2);
        assert_eq!(slave_reasserted[17] & (1 << 2), 1 << 2);
        assert_eq!(slave_reasserted[19] & (1 << 2), 0);
        assert_eq!(machine.pic.acknowledge(), Some(0x72));
        machine.pic.set_irq_level(10, false);
        write_pic_port(&mut machine, 0xa0, 0x20);
        write_pic_port(&mut machine, 0x20, 0x20);
        let settled = pic_payload(&machine);
        assert_eq!(settled[0] & (1 << 2), 0);
        assert_eq!(settled[1] & (1 << 2), 0);
        assert_eq!(settled[2] & (1 << 2), 0);
        assert_eq!(settled[17] & (1 << 2), 0);
        assert_eq!(settled[18] & (1 << 2), 0);
        assert_eq!(settled[19] & (1 << 2), 0);
    }

    #[test]
    fn pit_capture_preserves_status_and_count_latch_read_order() {
        fn configured() -> Machine {
            let mut machine = test_machine();
            write_pit_port(&mut machine, 0x43, 0x36);
            write_pit_port(&mut machine, 0x40, 0x34);
            write_pit_port(&mut machine, 0x40, 0x12);
            machine.advance_devices_clocks(1_000);
            write_pit_port(&mut machine, 0x43, 0xc2);
            machine.advance_devices_clocks(1_000);
            machine
        }

        let mut captured = configured();
        let mut twin = configured();
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);

        let frozen = pit_payload(&captured);
        assert_eq!(frozen.len(), PIT_PAYLOAD_LEN);
        assert_eq!(frozen, pit_payload(&captured));
        assert_eq!(frozen[13], 1);
        assert_eq!(frozen[16], 1);

        assert_eq!(read_pit_port(&mut captured, 0x40), frozen[17]);
        assert_eq!(read_pit_port(&mut twin, 0x40), frozen[17]);
        let after_status = pit_payload(&captured);
        assert_eq!(after_status[13], 1);
        assert_eq!(&after_status[16..18], &[0, 0]);
        assert_eq!(captured.pit, twin.pit);

        assert_eq!(read_pit_port(&mut captured, 0x40), frozen[14]);
        assert_eq!(read_pit_port(&mut twin, 0x40), frozen[14]);
        let half_read = pit_payload(&captured);
        assert_eq!(half_read, pit_payload(&captured));
        assert_eq!(half_read[13], 1);
        assert_eq!(half_read[14], 0);
        assert_eq!(half_read[15], frozen[15]);
        assert_eq!(half_read[19], 1);

        assert_eq!(read_pit_port(&mut captured, 0x40), frozen[15]);
        assert_eq!(read_pit_port(&mut twin, 0x40), frozen[15]);
        let consumed = pit_payload(&captured);
        assert_eq!(&consumed[13..16], &[0, 0, 0]);
        assert_eq!(consumed[19], 0);
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);
        assert_eq!(captured.pic, twin.pic);
    }

    #[test]
    fn pit_capture_preserves_the_exact_586_irq0_deadline() {
        fn configured() -> Machine {
            let mut machine = test_machine();
            machine.set_mode(GswMode::Gsw586);
            initialize_pic_pair(&mut machine, false);
            write_pit_port(&mut machine, 0x43, 0x34);
            write_pit_port(&mut machine, 0x40, 4);
            write_pit_port(&mut machine, 0x40, 0);
            machine
        }

        let mut captured = configured();
        let mut twin = configured();
        let pit_clocks = captured.clocks_until_timer0_irq().unwrap();
        let deadline = captured
            .timeline
            .cpu_clocks_until(
                crate::timeline::DeviceClock::Pit,
                pit_clocks,
                u64::from(crate::PIT_INPUT_HZ),
            )
            .unwrap();
        assert!(deadline > 1);

        let before = pit_payload(&captured);
        assert_eq!(before, pit_payload(&captured));
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);
        assert_eq!(captured.pic, twin.pic);

        captured.advance_devices_clocks(deadline - 1);
        twin.advance_devices_clocks(deadline - 1);
        assert!(!captured.pic.irr_bit(0));
        assert!(!twin.pic.irr_bit(0));
        assert_eq!(pit_payload(&captured), pit_payload(&twin));
        assert_eq!(
            machine_control_timing_payload(&captured),
            machine_control_timing_payload(&twin)
        );
        assert_eq!(pic_payload(&captured), pic_payload(&twin));

        captured.advance_devices_clocks(1);
        twin.advance_devices_clocks(1);
        assert!(captured.pic.irr_bit(0));
        assert!(twin.pic.irr_bit(0));
        assert_eq!(captured.pit, twin.pit);
        assert_eq!(captured.timeline, twin.timeline);
        assert_eq!(captured.pic, twin.pic);
        assert_eq!(pit_payload(&captured), pit_payload(&twin));
        assert_eq!(
            machine_control_timing_payload(&captured),
            machine_control_timing_payload(&twin)
        );
        assert_eq!(pic_payload(&captured), pic_payload(&twin));
        assert_eq!(captured.pic.acknowledge(), Some(0x20));
        assert_eq!(twin.pic.acknowledge(), Some(0x20));
    }

    #[test]
    fn pit_timeline_and_pic_payloads_match_split_586_advancement() {
        fn configured() -> Machine {
            let mut machine = test_machine();
            machine.set_mode(GswMode::Gsw586);
            initialize_pic_pair(&mut machine, false);
            write_pit_port(&mut machine, 0x43, 0x34);
            write_pit_port(&mut machine, 0x40, 7);
            write_pit_port(&mut machine, 0x40, 0);
            machine
        }

        let mut whole = configured();
        let mut split = configured();
        let pit_clocks = whole.clocks_until_timer0_irq().unwrap();
        let first_deadline = whole
            .timeline
            .cpu_clocks_until(
                crate::timeline::DeviceClock::Pit,
                pit_clocks,
                u64::from(crate::PIT_INPUT_HZ),
            )
            .unwrap();
        let total = first_deadline * 5 + 17;
        whole.advance_devices_clocks(total);
        let parts = [1, 17, first_deadline, first_deadline * 2];
        for clocks in parts {
            split.advance_devices_clocks(clocks);
        }
        split.advance_devices_clocks(total - parts.into_iter().sum::<u64>());

        assert!(whole.pic.irr_bit(0));
        assert_eq!(whole.pit, split.pit);
        assert_eq!(whole.timeline, split.timeline);
        assert_eq!(whole.pic, split.pic);
        assert_eq!(pit_payload(&whole), pit_payload(&split));
        assert_eq!(
            machine_control_timing_payload(&whole),
            machine_control_timing_payload(&split)
        );
        assert_eq!(pic_payload(&whole), pic_payload(&split));
    }

    #[test]
    fn non_ring_zero_pending_soft_int_is_resume_equivalent_to_none() {
        let rom = rom_with_code(&[0x90, 0xf4]);
        let profile = MachineProfile::gsw_386(16, VideoCard::Vega);
        let mut none = Machine::new(profile.clone(), &rom).unwrap();
        let mut residue = Machine::new(profile, &rom).unwrap();
        residue.pending_soft_int = Some(0x21);

        assert_eq!(
            machine_control_timing_payload(&none),
            machine_control_timing_payload(&residue)
        );
        let none_reason = none.run_until_halt_or_cycles(10_000).unwrap();
        let residue_reason = residue.run_until_halt_or_cycles(10_000).unwrap();

        assert_eq!(none_reason, residue_reason);
        assert!(none.cpu.perf_counters().instructions > 0);
        assert_eq!(none.cpu, residue.cpu);
        assert_eq!(none.timeline, residue.timeline);
        assert_eq!(none.memory.as_slice(), residue.memory.as_slice());
        assert_eq!(none.pic, residue.pic);
        assert_eq!(none.pit, residue.pit);
        assert_eq!(none.pending_soft_int, None);
        assert_eq!(residue.pending_soft_int, None);
        assert_eq!(none.elapsed_clocks, residue.elapsed_clocks);
        assert_eq!(none.io_stall_clocks, residue.io_stall_clocks);
        assert_eq!(none.trace.elapsed_clocks(), residue.trace.elapsed_clocks());
        assert_eq!(none.scaled_bus_clocks, residue.scaled_bus_clocks);
        assert_eq!(
            machine_control_timing_payload(&none),
            machine_control_timing_payload(&residue)
        );
    }

    #[test]
    fn ring_zero_pending_soft_int_remains_exact_state() {
        let mut none = test_machine();
        let mut pending = test_machine();
        none.cpu.control.cr0 |= 1;
        pending.cpu.control.cr0 |= 1;
        pending.pending_soft_int = Some(0x21);

        let none_payload = machine_control_timing_payload(&none);
        let pending_payload = machine_control_timing_payload(&pending);

        assert_eq!(&none_payload[7..9], &[0, 0]);
        assert_eq!(&pending_payload[7..9], &[1, 0x21]);
        assert_ne!(none_payload, pending_payload);
    }

    #[test]
    fn control_payload_excludes_other_owner_and_host_mechanism_state() {
        let mut machine = test_machine();
        let expected = machine_control_timing_payload(&machine);

        machine.profile.cpu = GswMode::Gsw586;
        machine.profile.address_pipelining = true;
        machine.profile.cache_enabled = true;
        machine.io_touched = true;
        machine.direct_mapping_epoch = 0x1234_5678;
        machine.host_profile.enable();
        machine.trace.set_tracing_mode(TracingMode::Counts);
        let _ = machine.cache_model.data_tier(GswMode::Gsw386, 0x4000);
        machine.katea_root = Some(std::path::PathBuf::from("ignored-host-path"));
        #[cfg(feature = "jit")]
        {
            machine.poll_skip_enabled = !machine.poll_skip_enabled;
            machine.poll_skip_diagnostics.enable_for_test();
        }

        assert_eq!(machine_control_timing_payload(&machine), expected);

        let mut switched = test_machine();
        let before_switch = machine_control_timing_payload(&switched);
        switched.set_mode(GswMode::Gsw586);
        assert_eq!(machine_control_timing_payload(&switched), before_switch);
    }

    #[test]
    fn deferred_services_are_rejected_independently() {
        for command in [
            unittester::CMD_CRC,
            unittester::CMD_SNAPSHOT,
            unittester::CMD_EXIT,
            0x7f,
        ] {
            let mut unit = test_machine();
            write_speaker_port(&mut unit, 0x03);
            let speaker_before = speaker_payload(&unit);
            let pit_before = pit_payload(&unit);
            assert!(
                unit.unittester
                    .write_port(unittester::PORT_COMMAND, command)
            );
            for _ in 0..2 {
                assert_eq!(
                    capture_error(&unit),
                    MachineCanonicalCaptureError::PendingUnitTesterCommand { command }
                );
                assert_eq!(unit.speaker.control_bits(), 3);
            }
            assert_eq!(unit.unittester.take_pending(), Some(command));
            assert_eq!(unit_tester_payload(&unit).len(), UNIT_TESTER_PAYLOAD_LEN);
            assert_eq!(speaker_payload(&unit), speaker_before);
            assert_eq!(pit_payload(&unit), pit_before);
        }

        let mut mode = test_machine();
        mode.pending_mode = Some(GswMode::Gsw586);
        assert_eq!(
            capture_error(&mode),
            MachineCanonicalCaptureError::PendingModeChange
        );

        let mut toka = test_machine();
        toka.pending_toka_service = Some(1);
        assert_eq!(
            capture_error(&toka),
            MachineCanonicalCaptureError::PendingTokaService { command: 1 }
        );

        let mut bios32 = test_machine();
        bios32.pending_bios32 = Some(Bios32Call::Directory);
        assert_eq!(
            capture_error(&bios32),
            MachineCanonicalCaptureError::PendingBios32Service
        );
    }

    #[test]
    fn uncommitted_coherence_is_rejected_independently() {
        let mut range = test_machine();
        range.pending_device_memory_write_range = Some((0x1000, 4));
        assert_eq!(
            capture_error(&range),
            MachineCanonicalCaptureError::PendingDeviceMemoryWriteRange
        );

        let mut coarse = test_machine();
        coarse.device_wrote_memory = true;
        assert_eq!(
            capture_error(&coarse),
            MachineCanonicalCaptureError::PendingDeviceMemoryWrite
        );

        let mut direct = test_machine();
        direct.direct_map_changed = true;
        assert_eq!(
            capture_error(&direct),
            MachineCanonicalCaptureError::PendingDirectMapChange
        );

        let mut data = test_machine();
        data.direct_data_map_changed = true;
        assert_eq!(
            capture_error(&data),
            MachineCanonicalCaptureError::PendingDirectDataMapChange
        );
    }

    #[test]
    fn uncommitted_batch_timing_is_rejected() {
        let mut machine = test_machine();
        machine.isa_io_batch_clocks = 17;
        assert_eq!(
            capture_error(&machine),
            MachineCanonicalCaptureError::UncommittedBatchTiming { clocks: 17 }
        );
    }

    #[test]
    fn uncommitted_console_publication_is_rejected() {
        let mut pending = test_machine();
        pending.program_output.extend_from_slice(b"pending");
        assert_eq!(
            capture_error(&pending),
            MachineCanonicalCaptureError::PendingConsolePublication { pending: 7 }
        );

        let mut invalid = test_machine();
        invalid.dos_screen_shown = 1;
        assert_eq!(
            capture_error(&invalid),
            MachineCanonicalCaptureError::InvalidConsoleTracker { shown: 1, total: 0 }
        );
    }

    #[test]
    fn inconsistent_machine_timing_is_rejected_independently() {
        let mut mode = test_machine();
        mode.cpu.set_mode(GswMode::Gsw586);
        assert_eq!(
            capture_error(&mode),
            MachineCanonicalCaptureError::InconsistentCpuMode {
                machine: GswMode::Gsw386,
                cpu: GswMode::Gsw586,
            }
        );

        let mut halted = test_machine();
        halted.halted_ticks = 1;
        assert_eq!(
            capture_error(&halted),
            MachineCanonicalCaptureError::InvalidHaltedTicks {
                halted_ticks: 1,
                now_ticks: 0,
            }
        );

        let mut stall = test_machine();
        stall.io_stall_clocks = 1;
        assert_eq!(
            capture_error(&stall),
            MachineCanonicalCaptureError::InvalidIoStallClocks {
                io_stall_clocks: 1,
                elapsed_clocks: 0,
            }
        );

        let mut bus = test_machine();
        bus.bus_rem = 31;
        assert_eq!(
            capture_error(&bus),
            MachineCanonicalCaptureError::InvalidBusRemainder {
                remainder: 31,
                denominator: 31,
            }
        );
    }

    #[test]
    fn inconsistent_modeled_cache_state_is_rejected_independently() {
        let mut l1_storage = test_machine();
        l1_storage.cache_model.l1_tags =
            vec![crate::CACHE_EMPTY_TAG; CACHE_L1_MAX_LINES - 1].into_boxed_slice();
        assert_eq!(
            capture_error(&l1_storage),
            MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
                tier: "L1",
                expected: CACHE_L1_MAX_LINES,
                actual: CACHE_L1_MAX_LINES - 1,
            }
        );

        let mut l2_storage = test_machine();
        l2_storage.cache_model.l2_tags =
            vec![crate::CACHE_EMPTY_TAG; CACHE_L2_MAX_LINES - 1].into_boxed_slice();
        assert_eq!(
            capture_error(&l2_storage),
            MachineCanonicalCaptureError::InvalidModeledCacheStorageLength {
                tier: "L2",
                expected: CACHE_L2_MAX_LINES,
                actual: CACHE_L2_MAX_LINES - 1,
            }
        );

        let expected_config = cache_level_config(GswMode::Gsw386);
        let mut l1_mask = test_machine();
        l1_mask.cache_model.config.l1_mask = 0;
        assert_eq!(
            capture_error(&l1_mask),
            MachineCanonicalCaptureError::InconsistentModeledCacheConfiguration {
                expected_l1: expected_config.l1_mask,
                actual_l1: 0,
                expected_l2: expected_config.l2_mask,
                actual_l2: expected_config.l2_mask,
            }
        );

        let mut l2_mask = test_machine();
        l2_mask.cache_model.config.l2_mask = 0x01ff;
        assert_eq!(
            capture_error(&l2_mask),
            MachineCanonicalCaptureError::InconsistentModeledCacheConfiguration {
                expected_l1: expected_config.l1_mask,
                actual_l1: expected_config.l1_mask,
                expected_l2: expected_config.l2_mask,
                actual_l2: 0x01ff,
            }
        );

        let expected_cost = tier_cost(GswMode::Gsw386);
        for (index, actual) in [[1, 0, 3], [0, 1, 3], [0, 0, 4]].into_iter().enumerate() {
            let mut machine = test_machine();
            machine.cache_model.cost.l1 = actual[0];
            machine.cache_model.cost.l2 = actual[1];
            machine.cache_model.cost.ram = actual[2];
            assert_eq!(
                capture_error(&machine),
                MachineCanonicalCaptureError::InconsistentModeledCacheCosts {
                    expected: [expected_cost.l1, expected_cost.l2, expected_cost.ram],
                    actual,
                },
                "cost component {index}"
            );
        }

        let mut code_fetch = test_machine();
        code_fetch.cache_model.code_fetch_ws = 1;
        assert_eq!(
            capture_error(&code_fetch),
            MachineCanonicalCaptureError::InconsistentModeledCodeFetchWaitStates {
                expected: code_fetch_ws(GswMode::Gsw386),
                actual: 1,
            }
        );
    }

    #[test]
    fn cpu_mode_mismatch_precedes_modeled_cache_validation() {
        let mut machine = test_machine();
        machine.cpu.set_mode(GswMode::Gsw586);
        machine.cache_model.config.l2_mask = 0;

        assert_eq!(
            capture_error(&machine),
            MachineCanonicalCaptureError::InconsistentCpuMode {
                machine: GswMode::Gsw386,
                cpu: GswMode::Gsw586,
            }
        );
    }

    #[test]
    fn cpu_capture_errors_keep_their_identity() {
        assert_eq!(
            MachineCanonicalCaptureError::from(CpuCanonicalCaptureError::ActiveRepContinuation),
            MachineCanonicalCaptureError::Cpu(CpuCanonicalCaptureError::ActiveRepContinuation)
        );
    }

    #[test]
    fn inconsistent_ram_rom_and_lookup_state_is_rejected_independently() {
        let expected_ram_len = usize::from(TEST_MEMORY_MIB) * 1024 * 1024;

        let mut ram = memory_test_machine();
        ram.profile.memory_mib += 1;
        assert_eq!(
            capture_error(&ram),
            MachineCanonicalCaptureError::InconsistentRamLength {
                expected: expected_ram_len + 1024 * 1024,
                actual: expected_ram_len,
            }
        );

        let mut rom = memory_test_machine();
        rom.rom.pop();
        assert_eq!(
            capture_error(&rom),
            MachineCanonicalCaptureError::InconsistentSystemRomLength {
                expected: BIOS_ROM_SIZE,
                actual: BIOS_ROM_SIZE - 1,
            }
        );

        let mut lookup = memory_test_machine();
        lookup.ram_lookup = crate::RamPageLookup::new(
            lookup.memory.len() - crate::video_params::RAM_LOOKUP_PAGE_SIZE,
            &lookup.vega,
        );
        assert_eq!(
            capture_error(&lookup),
            MachineCanonicalCaptureError::InconsistentRamPageLookup
        );
    }

    #[test]
    fn ram_rom_capture_and_serialization_are_read_only() {
        let mut machine = memory_test_machine();
        machine.memory.as_mut_slice()[0x1234] = 0xa5;
        machine.rom[0x4321] = 0x5a;
        machine.direct_mapping_epoch = 0x1234_5678;
        machine.trace.set_tracing_mode(TracingMode::Counts);
        let _ = machine
            .cache_model
            .data_tier(machine.active_mode, 0x0010_0000);

        let before_ram = machine.memory.as_slice().to_vec();
        let before_rom = machine.rom.clone();
        let before_cache = (
            machine.cache_model.l1_tags.clone(),
            machine.cache_model.l2_tags.clone(),
            machine.cache_model.lookups,
        );
        let before_mechanism = (
            machine.direct_mapping_epoch,
            machine.direct_map_changed,
            machine.direct_data_map_changed,
            machine.pending_device_memory_write_range,
            machine.device_wrote_memory,
        );
        let before_timing = (
            machine.elapsed_clocks,
            machine.scaled_bus_clocks,
            machine.trace.elapsed_clocks(),
        );

        let first = ram_rom_payload(&machine);
        let second = ram_rom_payload(&machine);

        assert_eq!(first, second);
        assert_eq!(machine.memory.as_slice(), before_ram);
        assert_eq!(machine.rom, before_rom);
        assert_eq!(
            (
                machine.cache_model.l1_tags.clone(),
                machine.cache_model.l2_tags.clone(),
                machine.cache_model.lookups,
            ),
            before_cache
        );
        assert_eq!(
            (
                machine.direct_mapping_epoch,
                machine.direct_map_changed,
                machine.direct_data_map_changed,
                machine.pending_device_memory_write_range,
                machine.device_wrote_memory,
            ),
            before_mechanism
        );
        assert_eq!(
            (
                machine.elapsed_clocks,
                machine.scaled_bus_clocks,
                machine.trace.elapsed_clocks(),
            ),
            before_timing
        );
        assert!(
            machine
                .ram_lookup
                .is_consistent(machine.memory.len(), &machine.vega)
        );
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[test]
    fn scalar_and_direct_native_stores_share_the_ram_owner() {
        const OFFSET: u32 = 0x6000;
        const VALUE: u32 = 0x1122_3344;
        const PROGRAM: &[u8] = &[
            0xb9, 0x64, 0x00, 0x00, 0x00, // mov ecx,100
            0xb8, 0x44, 0x33, 0x22, 0x11, // mov eax,11223344h
            0xa3, 0x00, 0x60, 0x00, 0x00, // store: mov [00006000h],eax
            0x83, 0xe9, 0x01, // sub ecx,1
            0x75, 0xf6, // jnz store
            0xf4, // hlt
        ];

        if !izarravm_cpu::native_backend_available() {
            return;
        }

        fn store_machine(program: &[u8], native: bool) -> Machine {
            let mut profile = MachineProfile::gsw_386(TEST_MEMORY_MIB, VideoCard::Vega);
            profile.cpu = GswMode::Gsw586;
            let mut machine = Machine::new_raw_program(profile, program).unwrap();
            let mut cs = machine.cpu.registers.cs();
            cs.default_size_32 = true;
            cs.limit = u32::MAX;
            machine.cpu.registers.set_segment(SegmentIndex::Cs, cs);
            machine.cpu.set_clif_backend_enabled(false);
            machine.cpu.set_native_backend_enabled(native);
            machine.set_jit_auto_admit(native);
            machine.trace.set_tracing_mode(TracingMode::Off);
            machine.poll_skip_enabled = false;
            machine
        }

        let mut scalar = store_machine(PROGRAM, false);
        let mut bulk_direct = store_machine(PROGRAM, false);
        let mut direct = store_machine(PROGRAM, true);
        let target = direct.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET;
        assert_eq!(
            scalar.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET,
            target
        );
        assert_eq!(
            bulk_direct.cpu.registers.segment(SegmentIndex::Ds).base + OFFSET,
            target
        );

        {
            let mut bus = scalar.make_bus();
            CpuBus::write_memory(
                &mut bus,
                target,
                BusWidth::Dword,
                VALUE,
                BusAccessKind::DataWrite,
            )
            .unwrap();
        }
        {
            let mut bus = bulk_direct.make_bus();
            assert_eq!(
                CpuBus::write_memory_bytes_direct(
                    &mut bus,
                    target,
                    &VALUE.to_le_bytes(),
                    BusWidth::Dword,
                    BusAccessKind::DataWrite,
                )
                .unwrap(),
                4
            );
        }

        let installed = direct.cpu.perf_counters().jit_direct_blocks_installed;
        let native_stores = direct.cpu.perf_counters().jit_native_store_hits;
        for _ in 0..4 {
            direct.cpu.halted = false;
            direct.cpu.registers.eip = 0x100;
            assert_eq!(
                direct.run_until_halt_or_cycles(100_000).unwrap(),
                StopReason::Halted
            );
        }

        assert!(direct.cpu.perf_counters().jit_direct_blocks_installed > installed);
        assert!(direct.cpu.perf_counters().jit_native_store_hits > native_stores);
        assert_eq!(
            &direct.memory.as_slice()[target as usize..target as usize + 4],
            &VALUE.to_le_bytes()
        );
        assert_eq!(direct.memory.as_slice(), scalar.memory.as_slice());
        assert_eq!(bulk_direct.memory.as_slice(), scalar.memory.as_slice());
        assert_eq!(direct.rom, scalar.rom);
        assert_eq!(bulk_direct.rom, scalar.rom);
        assert_eq!(ram_rom_payload(&direct), ram_rom_payload(&scalar));
        assert_eq!(ram_rom_payload(&bulk_direct), ram_rom_payload(&scalar));
    }

    #[test]
    fn unit_tester_exit_zero_is_a_captureable_batch_boundary() {
        let rom = rom_with_code(&[
            0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
            0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
            0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
            0xf4, // must not execute
        ]);
        let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::TestExit { code: 0 });
        assert_eq!(machine.cpu.registers.eip, 12);
        assert!(!machine.cpu.halted);
        assert!(machine.io_touched);
        assert!(!machine.cpu.is_ring0_protected());
        assert_eq!(machine.dos_screen_shown, machine.program_output.len());
        assert_eq!(
            machine_control_timing_payload(&machine).len(),
            MACHINE_CONTROL_TIMING_PAYLOAD_LEN
        );
        assert_eq!(
            modeled_cache_payload(&machine),
            vec![0; EMPTY_MODELED_CACHE_PAYLOAD_LEN]
        );
        assert_eq!(pic_payload(&machine).len(), PIC_PAYLOAD_LEN);
        let pit_before = machine.pit.clone();
        let timeline_before = machine.timeline;
        let pic_before = machine.pic.clone();
        let trace_before = machine.trace.elapsed_clocks();
        let first_pit = pit_payload(&machine);
        let second_pit = pit_payload(&machine);
        assert_eq!(first_pit.len(), PIT_PAYLOAD_LEN);
        assert_eq!(first_pit, second_pit);
        assert_eq!(machine.pit, pit_before);
        assert_eq!(machine.timeline, timeline_before);
        assert_eq!(machine.pic, pic_before);
        assert_eq!(machine.trace.elapsed_clocks(), trace_before);
        let first_dma = dma_payload(&machine);
        let first_dma_totals = dma_event_totals_v1_payload(&machine);
        let second_dma_totals = dma_event_totals_v1_payload(&machine);
        let second_dma = dma_payload(&machine);
        assert_eq!(first_dma.len(), DMA_PAYLOAD_LEN);
        assert_eq!(first_dma, second_dma);
        assert_eq!(first_dma_totals.len(), DMA_EVENT_TOTALS_V1_PAYLOAD_LEN);
        assert_eq!(first_dma_totals, second_dma_totals);
        assert_eq!(first_dma_totals, vec![0; DMA_EVENT_TOTALS_V1_PAYLOAD_LEN]);
        let first_rtc = rtc_payload(&machine);
        let second_rtc = rtc_payload(&machine);
        assert_eq!(first_rtc.len(), RTC_PAYLOAD_LEN);
        assert_eq!(first_rtc, second_rtc);
        let first_unit_tester = unit_tester_payload(&machine);
        let second_unit_tester = unit_tester_payload(&machine);
        assert_eq!(first_unit_tester.len(), UNIT_TESTER_PAYLOAD_LEN);
        assert_eq!(first_unit_tester, second_unit_tester);
        assert_eq!(first_unit_tester[0], unittester::REG_EXIT as u8 + 1);
        assert_eq!(first_unit_tester[1 + unittester::REG_EXIT], 0);
        let payload = ram_rom_payload(&machine);
        let mut cursor = 0;
        assert_eq!(
            take_len_prefixed_bytes(&payload, &mut cursor),
            machine.memory.as_slice()
        );
        assert_eq!(
            take_len_prefixed_bytes(&payload, &mut cursor),
            machine.rom.as_slice()
        );
        assert_eq!(cursor, payload.len());
    }

    #[test]
    fn speaker_latch_survives_a_real_586_test_exit_boundary() {
        let rom = rom_with_code(&[
            0xb0, 0xff, 0xe6, 0x61, // latch both speaker bits; upper bits normalize
            0xb0, 0x0c, 0xe6, 0xe4, // select REG_EXIT
            0xb0, 0x00, 0xe6, 0xe5, // write exit code zero
            0xb0, 0x03, 0xe6, 0xe6, // issue CMD_EXIT
            0xf4, // must not execute
        ]);
        let mut machine = Machine::new(MachineProfile::gsw_386(16, VideoCard::Vega), rom).unwrap();
        machine.set_mode(GswMode::Gsw586);

        let reason = machine.run_until_halt_or_cycles(1_000_000).unwrap();

        assert_eq!(reason, StopReason::TestExit { code: 0 });
        assert_eq!(machine.cpu.registers.eip, 16);
        assert!(!machine.cpu.halted);
        assert_eq!(machine.unittester.pending_command(), None);
        assert_eq!(speaker_payload(&machine), [3]);
        assert_eq!(pit_payload(&machine)[PIT_CHANNEL_2_GATE_OFFSET], 1);
        assert_eq!(read_speaker_port(&mut machine) & 0x03, 3);
        assert_eq!(speaker_payload(&machine), [3]);
    }
}
