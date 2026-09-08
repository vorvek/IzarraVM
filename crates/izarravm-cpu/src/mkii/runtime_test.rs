// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn install(engine: &mut Engine, cpu: &mut CpuGsw, eip: u32, physical: u32) -> Key {
    let insn = DecodedInsn {
        len: 1,
        prefixes: Prefixes::default(),
        opcode: 0x90,
        operand_size: OperandSize::Word,
        address_size: AddressSize::Word,
        modrm: None,
        operand: None,
        imm: 0,
        imm2: 0,
        group: DecodeGroup::DataMove,
        continuable: true,
        disp_len: 0,
        imm_len: 0,
    };
    let operations = vec![Operation::lower(eip, physical, insn).unwrap()].into_boxed_slice();
    let code = super::super::native::compile(&operations).unwrap();
    cpu.mkii_watch_source(physical, 1);
    let index = engine.free.pop().unwrap_or_else(|| {
        engine.arena.push(None);
        engine.arena.len() - 1
    });
    let key = Key {
        table: cpu.class_table() as *const _ as usize,
        physical,
        eip,
        cs_base: 0,
        cs_limit: 0xffff,
        cs_selector: 0,
        cpl: 0,
    };
    engine.traces.insert(key, index);
    engine.arena[index] = Some(Trace {
        operations,
        code,
        open_tail: None,
        source_certificate: None,
    });
    key
}

#[test]
fn mkii_dispatch_cache_checks_full_keys_and_forgets_retired_arena_slots() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    assert_eq!(engine.dispatch.capacity(), 0);
    let first = install(&mut engine, &mut cpu, 0x100, 0x100);
    let same_slot = (0x101u32..0x10000)
        .find(|physical| {
            physical.wrapping_mul(0x9e37_79b9) >> 20
                == first.physical.wrapping_mul(0x9e37_79b9) >> 20
        })
        .unwrap();
    let second = install(&mut engine, &mut cpu, same_slot, same_slot);
    let first_index = engine.lookup(first).unwrap();
    assert_eq!(engine.lookup(first), Some(first_index));
    let hits = engine.stats.dispatch_hits;
    let second_index = engine.lookup(second).unwrap();
    assert_ne!(first_index, second_index);
    assert_eq!(engine.stats.dispatch_hits, hits);
    assert_eq!(engine.lookup(first), Some(first_index));
    for alias in [
        Key { cpl: 3, ..first },
        Key {
            cs_selector: 8,
            ..first
        },
        Key {
            cs_base: 16,
            ..first
        },
        Key {
            cs_limit: 256,
            ..first
        },
        Key { table: 1, ..first },
    ] {
        assert_eq!(engine.lookup(alias), None);
        assert_eq!(engine.lookup(first), Some(first_index));
    }
    assert!(cpu.jit_direct.mkii.note_write(first.physical, 1));
    engine.drain_writes(&mut cpu);
    assert_eq!(engine.lookup(first), None);
    let replacement = install(&mut engine, &mut cpu, 0x300, 0x300);
    assert_eq!(engine.lookup(replacement), Some(first_index));
    assert_eq!(engine.lookup(first), None);
    assert_eq!(engine.lookup(second), Some(second_index));
    engine.clear(&mut cpu);
    assert!(engine.free.is_empty());
    assert!(engine.arena.is_empty());
    assert_eq!(engine.lookup(replacement), None);
    let _leased = std::mem::take(&mut engine);
    assert_eq!(engine.dispatch.capacity(), 0);
}

#[test]
fn mkii_narrow_retirement_preserves_shared_pages_and_removes_aliases() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    install(&mut engine, &mut cpu, 0x100, 0x100);
    install(&mut engine, &mut cpu, 0x200, 0x100);
    install(&mut engine, &mut cpu, 0x101, 0x101);
    install(&mut engine, &mut cpu, 0x300, 0x300);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x100), 2);
    assert!(cpu.jit_direct.mkii.note_write(0x100, 1));
    assert!(cpu.jit_direct.mkii.note_write(0x300, 1));
    engine.drain_writes(&mut cpu);
    assert_eq!(engine.traces.len(), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x101), 1);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x300), 0);
    assert!(!cpu.jit_direct.mkii.note_write(0x100, 1));
    assert!(cpu.jit_direct.mkii.note_write(0x101, 1));
    assert_eq!(engine.stats.retired_artifacts, 3);
    cpu.jit_direct.mkii.invalidate_code();
    engine.drain_writes(&mut cpu);
    engine.clear(&mut cpu);
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x101), 0);
    assert!(cpu.jit_direct.mkii.sources.is_empty());
    assert!(cpu.jit_direct.mkii.dirty_writes.is_empty());
    assert!(!cpu.jit_direct.mkii.full_flush);
}

#[test]
fn mkii_dirty_queue_overflow_promotes_to_a_bounded_full_flush() {
    let mut cpu = CpuGsw::default();
    let mut engine = Engine::default();
    install(&mut engine, &mut cpu, 0, 0x100);
    for _ in 0..1000 {
        assert!(cpu.jit_direct.mkii.note_write(0x100, 1));
    }
    assert!(cpu.jit_direct.mkii.full_flush);
    assert!(cpu.jit_direct.mkii.dirty_writes.is_empty());
    engine.drain_writes(&mut cpu);
    assert!(engine.traces.is_empty());
    assert_eq!(cpu.jit_direct.code_watch.refcount(0x100), 0);
    assert!(!cpu.jit_direct.mkii.code_dirty);
}
