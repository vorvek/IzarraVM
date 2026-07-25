// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::{CanonicalFieldWriter, CanonicalSectionRequirement, CanonicalStateError};
use izarravm_cpu::{CanonicalCpuExecution, CpuCanonicalCaptureError, bus_timing};
use thiserror::Error;

use crate::{
    CacheModel, Machine,
    ata::CanonicalAtaDisk,
    bmide::CanonicalBusMasterIde,
    cache_config::{
        CACHE_L1_MAX_LINES, CACHE_L2_MAX_LINES, CACHE_LINE_BYTES, CACHE_TIER_DISABLED_MASK,
        cache_level_config, code_fetch_ws, tier_cost,
    },
    dma::{CanonicalDma8237Pair, CanonicalDmaEventTotalsV1},
    ide::CanonicalIdeChannel,
    pci::CanonicalPciConfig,
    pic::CanonicalPic8259Pair,
    pit::CanonicalPit,
    rtc::CanonicalRtc,
    speaker::CanonicalSpeaker,
    timeline::{CanonicalTimelineError, CanonicalTimelineProjection},
    unittester::CanonicalUnitTester,
    vega::CanonicalVega,
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
const STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS: [StateSnapshotV1FoundationSection; 18] = [
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
    StateSnapshotV1FoundationSection {
        id: 0x0002_000a,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_000b,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_000c,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_000d,
        version: 1,
        requirement: CanonicalSectionRequirement::Required,
    },
    StateSnapshotV1FoundationSection {
        id: 0x0002_000e,
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
    assert!(
        sections[13].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[14].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[15].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[16].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
            == STATE_SNAPSHOT_V1_MACHINE_NAMESPACE
    );
    assert!(
        sections[17].id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK
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
    assert!(sections[12].id < sections[13].id);
    assert!(sections[13].id < sections[14].id);
    assert!(sections[14].id < sections[15].id);
    assert!(sections[15].id < sections[16].id);
    assert!(sections[16].id < sections[17].id);
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
    #[error(
        "the Distira init-enable mirror {mirror:#010x} disagrees with the Vega latch {latch:#010x}"
    )]
    InconsistentDistiraInitEnableMirror { latch: u32, mirror: u32 },
    #[error("ATA cylinder count {cylinders} disagrees with derived geometry {expected}")]
    InconsistentAtaGeometry { cylinders: u32, expected: u32 },
    #[error("a BMIDE primary transfer is active without an armed ATA DMA request")]
    DanglingBmideTransfer,
    #[error("the test-only ATAPI packet stall seam is armed")]
    TestStallPacketEnabled,
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
    pci: CanonicalPciConfig<'a>,
    vega: CanonicalVega<'a>,
    ata: CanonicalAtaDisk<'a>,
    bmide: CanonicalBusMasterIde<'a>,
    atapi_channel: CanonicalIdeChannel<'a>,
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

    /// Writes the required mechanism-1 selector and PIIX IDE owner payload.
    ///
    /// Distira configuration and BMIDE transfer continuation remain in their
    /// respective device owners rather than being duplicated here.
    #[allow(dead_code)]
    pub(crate) fn write_pci_config_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.pci.write_payload(out)
    }

    /// Writes the required Vega outer-routing owner payload.
    ///
    /// The six latches are the guest-programmed routing state Vega owns
    /// directly. Vga, Margo, and Distira internals, including the init-enable
    /// mirror Distira keeps, belong to their own future owners; capture
    /// rejects a drifted mirror before this payload can be written.
    #[allow(dead_code)]
    pub(crate) fn write_vega_routing_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.vega.write_payload(out)
    }

    /// Writes the required ATA primary-channel controller owner payload.
    ///
    /// Task-file registers, latches, and the transfer continuation only.
    /// Media content belongs to the future HDD-content owner, and BMIDE
    /// transfer continuation to the BMIDE owner. Capture cross-checks the
    /// mount-derived cylinder count instead of serializing it.
    #[allow(dead_code)]
    pub(crate) fn write_ata_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.ata.write_payload(out)
    }

    /// Writes the required BMIDE bus-master owner payload.
    ///
    /// Raw channel registers plus the parsed transfer continuation. Capture
    /// rejects a primary transfer whose armed ATA DMA request has vanished
    /// before this payload can be written.
    #[allow(dead_code)]
    pub(crate) fn write_bmide_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.bmide.write_payload(out)
    }

    /// Writes the required secondary-channel ATAPI owner payload.
    ///
    /// Channel registers, packet/data continuation, and the device latches.
    /// Disc content belongs to the future CD-content owner, and the host
    /// mixer cursors are presentation state, not guest state. Capture
    /// rejects the test-only packet stall seam before this payload can be
    /// written.
    #[allow(dead_code)]
    pub(crate) fn write_atapi_channel_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        self.atapi_channel.write_payload(out)
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
        let (init_enable_latch, init_enable_mirror) = self.vega.distira_init_enable_mirror();
        if init_enable_latch != init_enable_mirror {
            return Err(
                MachineCanonicalCaptureError::InconsistentDistiraInitEnableMirror {
                    latch: init_enable_latch,
                    mirror: init_enable_mirror,
                },
            );
        }
        if let Some(disk) = self.ata.as_ref() {
            // total_sectors() caps at 2^28-1 while mount derived cylinders
            // from the uncapped count, so images of 128 GiB or more fail
            // here; that pathological case is rejected on purpose.
            let expected = (disk.total_sectors() / (16 * 63)).max(1);
            if disk.cylinders() != expected {
                return Err(MachineCanonicalCaptureError::InconsistentAtaGeometry {
                    cylinders: disk.cylinders(),
                    expected,
                });
            }
        }
        if self.bmide.ticks_until_completion().is_some()
            && self
                .ata
                .as_ref()
                .and_then(crate::ata::AtaDisk::pending_dma)
                .is_none()
        {
            return Err(MachineCanonicalCaptureError::DanglingBmideTransfer);
        }
        if self.ide.test_stall_packet_enabled() {
            return Err(MachineCanonicalCaptureError::TestStallPacketEnabled);
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
        let pci = self.pci.canonical_projection();
        let vega = self.vega.canonical_projection();
        let ata = CanonicalAtaDisk::new(self.ata.as_ref());
        let bmide = self.bmide.canonical_projection();
        let atapi_channel = self.ide.canonical_projection();
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
            pci,
            vega,
            ata,
            bmide,
            atapi_channel,
        })
    }
}

#[cfg(test)]
#[path = "canonical_state_test.rs"]
mod tests;
