// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn fixture(code: &[u8]) -> (CpuGsw, TestBus) {
    let (mut cpu, memory) = real_mode_cpu(code, 0x2000);
    cpu.set_mode(GswMode::Gsw586);
    let mut bus = TestBus::with_memory(memory);
    bus.code_fetch_observations = Some(Vec::new());
    (cpu, bus)
}

fn charges(bus: &TestBus) -> Vec<u32> {
    bus.trace
        .cycles()
        .iter()
        .filter(|cycle| cycle.kind == BusAccessKind::InstructionPrefetch)
        .map(|cycle| cycle.address)
        .collect()
}

#[test]
fn opcode_fetch_consumes_each_prefix_and_opcode_byte_once() {
    for (code, opcode) in [
        (&[0x90][..], 0x90),
        (&[0x66, 0x66, 0x67, 0x67, 0x2e, 0x90][..], 0x90),
        (&[0x0f, 0xa2][..], 0x0fa2),
        (&[0x66, 0x0f, 0xb6, 0xc4][..], 0x0fb6),
        (&[0xb8, 0x34, 0x12][..], 0xb8),
    ] {
        let (mut cpu, mut bus) = fixture(code);
        let decoded = cpu.decode(&mut bus).unwrap();
        let expected: Vec<_> = (0..code.len() as u32).collect();
        assert_eq!(decoded.opcode, opcode);
        assert_eq!(usize::from(decoded.len), code.len());
        assert_eq!(cpu.registers.eip, code.len() as u32);
        assert_eq!(charges(&bus), expected);
        assert_eq!(bus.code_fetch_observations.as_ref().unwrap(), &expected);
    }
}

#[test]
fn opcode_fetch_faults_preserve_only_the_completed_byte_prefix() {
    for (code, fail, expected) in [
        (&[0x90][..], 0, vec![]),
        (&[0x66, 0x90][..], 1, vec![0]),
        (&[0xb8, 0x34, 0x12][..], 2, vec![0, 1]),
        (&[0x0f, 0xa2][..], 1, vec![0]),
    ] {
        let (mut cpu, mut bus) = fixture(code);
        bus.fail_fetch_charge_at = Some(fail);
        assert!(matches!(
            cpu.decode(&mut bus),
            Err(InternalFault::Cpu(CpuError::Bus(_)))
        ));
        assert_eq!(cpu.registers.eip, fail);
        assert_eq!(charges(&bus), expected);
        assert_eq!(
            bus.code_fetch_observations.as_ref().unwrap(),
            &(0..=fail).collect::<Vec<_>>()
        );
    }

    let (mut cpu, mut bus) = fixture(&[0x66, 0xb8, 0x34, 0x12]);
    let mut cs = cpu.registers.cs();
    cs.limit = 1;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    assert!(matches!(
        cpu.decode(&mut bus),
        Err(InternalFault::Exception { vector: 13, .. })
    ));
    assert_eq!(cpu.registers.eip, 2);
    assert_eq!(charges(&bus), vec![0, 1]);
    assert_eq!(bus.code_fetch_observations.unwrap(), vec![0, 1]);

    let (mut cpu, mut bus) = fixture(&[]);
    bus.memory = vec![0; 0x1000].into();
    bus.memory[0xfff] = 0xb8;
    cpu.registers.eip = 0xfff;
    assert!(matches!(
        cpu.decode(&mut bus),
        Err(InternalFault::Cpu(CpuError::Bus(_)))
    ));
    assert_eq!(cpu.registers.eip, 0x1000);
    assert_eq!(charges(&bus), vec![0xfff]);
    assert_eq!(bus.code_fetch_observations.unwrap(), vec![0xfff, 0x1000]);
}

#[test]
fn opcode_fetch_keeps_independent_lock_validation_peeks() {
    for (code, expected) in [
        (&[0xf0, 0x01, 0x06, 0x20, 0x00][..], vec![0, 1, 2, 2, 3, 4]),
        (
            &[0xf0, 0x0f, 0xb1, 0x06, 0x20, 0x00][..],
            vec![0, 1, 2, 3, 2, 3, 4, 5],
        ),
    ] {
        let (mut cpu, mut bus) = fixture(code);
        let decoded = cpu.decode(&mut bus).unwrap();
        assert!(decoded.prefixes.lock);
        assert_eq!(cpu.registers.eip, code.len() as u32);
        assert_eq!(charges(&bus), expected);
        assert_eq!(
            bus.code_fetch_observations.unwrap(),
            (0..code.len() as u32).collect::<Vec<_>>()
        );
    }
    let (mut cpu, mut bus) = fixture(&[0xf0, 0x01, 0xc0]);
    assert!(matches!(
        cpu.decode(&mut bus),
        Err(InternalFault::Exception { vector: 6, .. })
    ));
    assert_eq!(cpu.registers.eip, 2);
    assert_eq!(charges(&bus), vec![0, 1, 2]);
    assert_eq!(bus.code_fetch_observations.unwrap(), vec![0, 1]);
}

#[test]
fn opcode_fetch_cold_and_cached_iret_charge_one_rom_byte() {
    let (mut cpu, mut bus) = fixture(&[]);
    bus.memory = vec![0; 0x100000].into();
    bus.memory[0xff25f] = 0xcf;
    cpu.load_segment_real(SegmentIndex::Cs, 0xf000);
    for cold in [true, false, false] {
        cpu.registers.eip = 0xf25f;
        bus.trace.clear();
        bus.code_fetch_observations.as_mut().unwrap().clear();
        let misses = cpu.perf.decode_misses;
        let decoded = cpu.fetch_decoded(&mut bus, 0xff25f).unwrap();
        assert_eq!(decoded.opcode, 0xcf);
        assert_eq!(cpu.perf.decode_misses - misses, u64::from(cold));
        assert_eq!(cpu.registers.eip, 0xf260);
        assert_eq!(charges(&bus), vec![0xff25f]);
        assert_eq!(bus.code_fetch_observations.as_ref().unwrap(), &[0xff25f]);
    }
}
