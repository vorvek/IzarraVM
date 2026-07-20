// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::CanonicalSectionRequirement;
use izarravm_cpu::{CanonicalCpuExecution, CpuCanonicalCaptureError};
use thiserror::Error;

use crate::Machine;

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
const STATE_SNAPSHOT_V1_FOUNDATION_SECTIONS: [StateSnapshotV1FoundationSection; 4] = [
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
    assert!(sections[0].id < sections[1].id);
    assert!(sections[1].id < sections[2].id);
    assert!(sections[2].id < sections[3].id);
    assert!(sections[3].id < STATE_SNAPSHOT_V1_MACHINE_NAMESPACE);
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
        let cpu_execution = self.cpu.canonical_execution_capture()?;
        Ok(CanonicalMachineStateCapture {
            _machine: self,
            _cpu_execution: cpu_execution,
        })
    }
}

#[cfg(test)]
mod tests {
    use izarravm_core::{CanonicalSectionRequirement, GswMode, VideoCard};
    use izarravm_cpu::CpuCanonicalCaptureError;

    use super::*;
    use crate::{BIOS_ROM_SIZE, Bios32Call, MachineProfile, StopReason, unittester};

    fn test_machine() -> Machine {
        Machine::new(
            MachineProfile::gsw_386(16, VideoCard::Vega),
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
        assert!(sections[1..].iter().all(|section| {
            section.id & STATE_SNAPSHOT_V1_OWNER_NAMESPACE_MASK == STATE_SNAPSHOT_V1_CPU_NAMESPACE
        }));
        assert!(sections[3].id < STATE_SNAPSHOT_V1_MACHINE_NAMESPACE);
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

        let capture = machine.canonical_state_capture().unwrap();
        drop(capture);

        assert_eq!(machine.cpu, before_cpu);
        assert_eq!(machine.timeline, before_timeline);
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
    fn batch_scratch_and_pending_semantic_state_remain_captureable() {
        let mut machine = test_machine();
        machine.io_touched = true;
        machine.pending_soft_int = Some(0x21);
        machine.pic.request(5);

        let capture = machine.canonical_state_capture().unwrap();
        drop(capture);

        assert!(machine.io_touched);
        assert_eq!(machine.pending_soft_int, Some(0x21));
        assert!(machine.pic.irr_bit(5));
    }

    #[test]
    fn deferred_services_are_rejected_independently() {
        let mut unit = test_machine();
        assert!(
            unit.unittester
                .write_port(unittester::PORT_COMMAND, unittester::CMD_CRC)
        );
        assert_eq!(
            capture_error(&unit),
            MachineCanonicalCaptureError::PendingUnitTesterCommand {
                command: unittester::CMD_CRC
            }
        );

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
    fn cpu_capture_errors_keep_their_identity() {
        assert_eq!(
            MachineCanonicalCaptureError::from(CpuCanonicalCaptureError::ActiveRepContinuation),
            MachineCanonicalCaptureError::Cpu(CpuCanonicalCaptureError::ActiveRepContinuation)
        );
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
        assert!(machine.io_touched);
        assert!(!machine.cpu.is_ring0_protected());
        assert!(machine.canonical_state_capture().is_ok());
    }
}
