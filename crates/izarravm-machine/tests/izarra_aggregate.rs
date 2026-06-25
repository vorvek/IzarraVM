//! Phase 3 aggregate: with every stream landed, the full POST result block must
//! stay self-consistent (the live-append header keeps the declared count in sync)
//! and every present component must PASS (a capability-revealing POST never FAILs
//! a device that is there). Guards against one stream's records desynchronizing
//! the shared block.

use izarravm_core::VideoCard;
use izarravm_firmware::{SuiteRecordStatus, izarra_bios, parse_result_block};
use izarravm_machine::{Machine, MachineProfile, StopReason};

#[test]
fn full_post_block_is_consistent_and_every_component_passes() {
    let mut machine = Machine::new(
        MachineProfile::gsw_386(16, VideoCard::Et4000Ax),
        izarra_bios(),
    )
    .unwrap();
    // Mount a real ATA disk so the HDD probe (component.disk_hdd) finds a device:
    // this is the fully-equipped machine where every component is present and must
    // PASS. (On a bare machine C: is HLE-backed, so the HDD probe FAILs by design.)
    machine.mount_hdd(vec![0u8; 64 * 512]);
    // The full-screen RLE POST background delays the step loop to ~10M cycles, so
    // give POST room to append every record before sampling the block.
    let stop = machine.run_until_halt_or_cycles(20_000_000).unwrap();
    assert!(
        matches!(stop, StopReason::CycleLimit { .. }),
        "POST completes and the BIOS idles (it does not halt)"
    );

    // parse_result_block validates the additive checksum; the declared count must
    // equal the parsed count for the live-append header to be correct.
    let results = parse_result_block(machine.memory().as_slice()).unwrap();
    assert_eq!(
        usize::from(results.declared_record_count),
        results.records.len(),
        "declared record count matches the parsed records"
    );

    // Every memory.* / component.* status record (not the MEASURE values) is PASS.
    for record in &results.records {
        let is_status = record.name.starts_with("memory.") || record.name.starts_with("component.");
        if is_status && record.status != SuiteRecordStatus::Measure {
            assert_eq!(
                record.status,
                SuiteRecordStatus::Pass,
                "record {} should PASS",
                record.name
            );
        }
    }

    // The full expected component/memory set is present.
    let names: Vec<&str> = results.records.iter().map(|r| r.name.as_str()).collect();
    for expected in [
        "memory.ramtest",
        "memory.detected_kib",
        "component.cpu_lotura",
        "cpu.gsw_mode",
        "component.kbd_8042",
        "component.timer_pit",
        "component.serial_com1",
        "component.audio_sbdsp",
        "sound.dsp_version",
        "component.audio_opl",
        "component.video_margo",
        "video.margo_caps",
        "component.cpu_gsw",
        "component.floppy_fdc",
        "component.disk_hdd",
        "component.optical_atapi",
    ] {
        assert!(names.contains(&expected), "missing record {expected}");
    }
}
