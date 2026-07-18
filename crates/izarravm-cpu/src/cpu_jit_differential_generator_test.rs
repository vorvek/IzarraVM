// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const CASES_PER_MODE: u32 = 32;
const MEMORY_LEN: usize = 0x20_000;

#[derive(Debug)]
struct GeneratedCase {
    seed: u64,
    entry: u32,
    bytes: Vec<u8>,
    gpr: [u32; 8],
    eflags: u32,
    cap: u64,
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn u32(&mut self) -> u32 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value as u32
    }

    fn reg(&mut self) -> u8 {
        (self.u32() & 7) as u8
    }
}

fn push_u32(code: &mut Vec<u8>, value: u32) {
    code.extend_from_slice(&value.to_le_bytes());
}

fn generated_case(index: u32, mode_offset: u32) -> GeneratedCase {
    let seed = 0xd1ff_e2e0_4865_0001u64
        ^ u64::from(index + mode_offset).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut rng = Rng::new(seed);
    let entry = 0x1000 + index * 0x100;
    let data = 0x1_0000 + index * 0x40;
    let op = ((index + mode_offset) & 7) as u8;
    let byte_lane = ((index + mode_offset) & 7) as u8;
    let memory_target = match index & 15 {
        0 => 0x1_8001, // isolated, unaligned page: the native map is deliberately cold
        1 => 0x1_8fff, // dword straddles two pages
        _ => data,
    };
    let mut bytes = Vec::with_capacity(128);

    // The starter is interpreted. The generated block begins at entry + 1 and ends at Jcc.
    bytes.push(0x90);

    let dst = rng.reg();
    bytes.push(0xb8 + dst);
    push_u32(&mut bytes, rng.u32());

    bytes.extend_from_slice(&[0xb0 + byte_lane, rng.u32() as u8]);
    bytes.extend_from_slice(&[0x88, 0xc0 | (((byte_lane + 4) & 7) << 3) | byte_lane]);
    bytes.extend_from_slice(&[0x8a, 0xc0 | (byte_lane << 3) | ((byte_lane + 4) & 7)]);

    let lea_dst = rng.reg();
    let scale = (rng.u32() & 3) as u8;
    bytes.extend_from_slice(&[0x8d, 0x84 | (lea_dst << 3), (scale << 6) | (6 << 3) | 3]);
    push_u32(&mut bytes, rng.u32() & 0xff);

    bytes.extend_from_slice(&[(op << 3) | 1, 0xc0 | (rng.reg() << 3) | rng.reg()]);
    bytes.push((op << 3) | 5);
    push_u32(&mut bytes, rng.u32());

    bytes.extend_from_slice(&[0x81, 0xc0 | (((op + 3) & 7) << 3) | rng.reg()]);
    push_u32(&mut bytes, rng.u32());
    bytes.extend_from_slice(&[
        0x83,
        0xc0 | (((op + 5) & 7) << 3) | rng.reg(),
        rng.u32() as u8,
    ]);
    bytes.extend_from_slice(&[
        0x80,
        0xc0 | (((op + 1) & 7) << 3) | rng.reg(),
        rng.u32() as u8,
    ]);

    bytes.extend_from_slice(&[0x85, 0xc0 | (rng.reg() << 3) | rng.reg()]);
    let shift = [4, 5, 7][(rng.u32() % 3) as usize];
    bytes.extend_from_slice(&[
        0xc1,
        0xc0 | (shift << 3) | rng.reg(),
        1 + (rng.u32() % 31) as u8,
    ]);
    bytes.push(if rng.u32() & 1 == 0 {
        0x40 + rng.reg()
    } else {
        0x48 + rng.reg()
    });

    bytes.extend_from_slice(&[0x8b, 0x05 | (rng.reg() << 3)]);
    push_u32(&mut bytes, memory_target);
    bytes.extend_from_slice(&[0x8a, 0x05 | (rng.reg() << 3)]);
    push_u32(&mut bytes, data + 8);
    bytes.extend_from_slice(&[0x89, 0x05 | (rng.reg() << 3)]);
    push_u32(&mut bytes, data + 12);
    bytes.extend_from_slice(&[0x88, 0x05 | (rng.reg() << 3)]);
    push_u32(&mut bytes, data + 16);

    bytes.extend_from_slice(&[0xc7, 0x05]);
    push_u32(&mut bytes, data + 20);
    push_u32(&mut bytes, rng.u32());
    bytes.extend_from_slice(&[0xc6, 0x05]);
    push_u32(&mut bytes, data + 24);
    bytes.push(rng.u32() as u8);

    bytes.extend_from_slice(&[((op << 3) | 3), 0x05 | (rng.reg() << 3)]);
    push_u32(&mut bytes, data + 28);
    bytes.push(0xa1);
    push_u32(&mut bytes, data + 32);
    bytes.push(0xa3);
    push_u32(&mut bytes, data + 36);

    // TEST defines every flag the terminal condition can consume.
    bytes.extend_from_slice(&[0x85, 0xc0]);
    let condition = ((index + mode_offset) & 15) as u8;
    if index & 1 == 0 {
        bytes.extend_from_slice(&[0x70 | condition, 1]);
    } else {
        bytes.extend_from_slice(&[0x0f, 0x80 | condition]);
        push_u32(&mut bytes, 1);
    }
    bytes.extend_from_slice(&[0xf4, 0xf4]);

    let mut gpr = [0; 8];
    for value in &mut gpr {
        *value = rng.u32();
    }
    gpr[4] = 0x1_f000;
    let arithmetic_flags = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
    let eflags = 0x202 | (rng.u32() & arithmetic_flags);

    GeneratedCase {
        seed,
        entry,
        bytes,
        gpr,
        eflags,
        cap: 256 + u64::from(rng.u32() & 3) * 128,
    }
}

fn generated_cpu(mode: GswMode) -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(mode);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0008, 0x9b));
    for segment in [
        SegmentIndex::Ds,
        SegmentIndex::Ss,
        SegmentIndex::Es,
        SegmentIndex::Fs,
        SegmentIndex::Gs,
    ] {
        cpu.registers
            .set_segment(segment, SegmentRegister::flat(0x0010, 0x93));
    }
    cpu
}

fn arm(cpu: &mut CpuGsw, case: &GeneratedCase) {
    cpu.halted = false;
    cpu.interrupt_shadow = false;
    cpu.registers.gpr = case.gpr;
    cpu.registers.eflags = case.eflags;
    cpu.pending_flags = PendingFlags::default();
    cpu.set_eip(case.entry);
    cpu.elapsed_clocks = 0;
    cpu.core_clocks_so_far = 0;
    cpu.timing_rem = 0;
    cpu.fp_rem = 0;
    cpu.fpu.finit();
    cpu.fpu.push(1.25);
    cpu.fpu.push(-2.5);
}

fn restore_bus(bus: &mut TestBus, pristine: &[u8]) {
    bus.memory.copy_from_slice(pristine);
    bus.trace.clear();
    bus.pending_irq = None;
    bus.io_touched = false;
}

fn run_to_halt<B: CpuBus>(
    cpu: &mut CpuGsw,
    bus: &mut B,
    case: &GeneratedCase,
) -> Result<Vec<BudgetedRunOutcome>, CpuError> {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        let outcome = cpu.run_budgeted(bus, case.cap)?;
        outcomes.push(outcome);
        if outcome.halted {
            return Ok(outcomes);
        }
    }
    panic!("generated guest did not halt: {case:#?}")
}

fn prime_direct(cpu: &mut CpuGsw, bus: &mut TestBus, pristine: &[u8], case: &GeneratedCase) {
    cpu.set_jit_auto_admit(false);
    restore_bus(bus, pristine);
    arm(cpu, case);
    run_to_halt(cpu, bus, case).unwrap();
    cpu.set_jit_auto_admit(true);
    let blocks = cpu.jit_direct.len();
    for _ in 0..4 {
        restore_bus(bus, pristine);
        arm(cpu, case);
        run_to_halt(cpu, bus, case).unwrap();
    }
    assert!(
        cpu.jit_direct.len() > blocks,
        "generated block did not compile: seed={:#x}, bytes={:02x?}, case={case:#?}, perf={:#?}",
        case.seed,
        case.bytes,
        cpu.perf_counters()
    );
}

fn run_generated_mode(mode: GswMode, mode_offset: u32) {
    let cases: Vec<_> = (0..CASES_PER_MODE)
        .map(|index| generated_case(index, mode_offset))
        .collect();
    let mut pristine = vec![0; MEMORY_LEN];
    let mut fill = Rng::new(0x7265_7072_6f64_7563 ^ u64::from(mode_offset));
    for byte in &mut pristine {
        *byte = fill.u32() as u8;
    }
    for case in &cases {
        let start = case.entry as usize;
        pristine[start..start + case.bytes.len()].copy_from_slice(&case.bytes);
    }

    let mut interpreter = generated_cpu(mode);
    let mut direct = generated_cpu(mode);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    for case in &cases {
        restore_bus(&mut interpreter_bus, &pristine);
        restore_bus(&mut direct_bus, &pristine);
        arm(&mut interpreter, case);
        // Decode and populate identical RAM mappings before hotness admission.
        run_to_halt(&mut interpreter, &mut interpreter_bus, case).unwrap();
        prime_direct(&mut direct, &mut direct_bus, &pristine, case);

        restore_bus(&mut interpreter_bus, &pristine);
        restore_bus(&mut direct_bus, &pristine);
        arm(&mut interpreter, case);
        arm(&mut direct, case);
        let before = direct.perf_counters().clone();
        let expected_fpu = interpreter.fpu.clone();

        let interpreted = run_to_halt(&mut interpreter, &mut interpreter_bus, case);
        let native = run_to_halt(&mut direct, &mut direct_bus, case);

        assert_eq!(native, interpreted, "run outcome differs: {case:#?}");
        assert_eq!(direct.registers, interpreter.registers, "{case:#?}");
        assert_eq!(direct.registers.eip, interpreter.registers.eip, "{case:#?}");
        assert_eq!(direct.eflags(), interpreter.eflags(), "{case:#?}");
        assert_eq!(direct.fpu, expected_fpu, "direct x87 changed: {case:#?}");
        assert_eq!(
            interpreter.fpu, expected_fpu,
            "interpreter x87 changed: {case:#?}"
        );
        assert_eq!(direct, interpreter, "full CPU state differs: {case:#?}");
        assert_eq!(direct_bus.memory, interpreter_bus.memory, "{case:#?}");
        assert_eq!(
            direct.elapsed_clocks, interpreter.elapsed_clocks,
            "{case:#?}"
        );
        assert_eq!(
            direct_bus.trace.elapsed_clocks(),
            interpreter_bus.trace.elapsed_clocks(),
            "bus clocks differ: {case:#?}"
        );
        assert!(
            direct.perf_counters().jit_direct_insns > before.jit_direct_insns,
            "accepted seed retired no native instructions: {case:#?}, perf={:#?}",
            direct.perf_counters()
        );
    }
}

#[test]
fn generated_direct_blocks_match_interpreter_in_486_and_586_modes() {
    run_generated_mode(GswMode::Gsw486, 0);
    run_generated_mode(GswMode::Gsw586, CASES_PER_MODE);
}

/// Track C C1a's third differential arm: the same generator suite routed through the clif
/// side-exit-shell policy instead of Direct. A C1a shell never retires a guest instruction
/// natively (F-A1 option B), so every path, guard-reject or guard-pass, ends with the
/// interpreter retiring the current instruction; state and timing must therefore be
/// BYTE-IDENTICAL to the plain interpreter run, not merely equal on the architectural fields
/// Direct's own assertions check. A mismatch here indicts the admission/guard/dispatch/
/// exit-state layer, since no lowering exists yet to blame instead.
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn prime_clif(cpu: &mut CpuGsw, bus: &mut TestBus, pristine: &[u8], case: &GeneratedCase) {
    cpu.set_clif_backend_enabled(false);
    restore_bus(bus, pristine);
    arm(cpu, case);
    run_to_halt(cpu, bus, case).unwrap();
    cpu.set_clif_backend_enabled(true);
    for _ in 0..4 {
        restore_bus(bus, pristine);
        arm(cpu, case);
        run_to_halt(cpu, bus, case).unwrap();
    }
}

#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn run_generated_mode_clif(mode: GswMode, mode_offset: u32) {
    let cases: Vec<_> = (0..CASES_PER_MODE)
        .map(|index| generated_case(index, mode_offset))
        .collect();
    let mut pristine = vec![0; MEMORY_LEN];
    let mut fill = Rng::new(0x7265_7072_6f64_7563 ^ u64::from(mode_offset));
    for byte in &mut pristine {
        *byte = fill.u32() as u8;
    }
    for case in &cases {
        let start = case.entry as usize;
        pristine[start..start + case.bytes.len()].copy_from_slice(&case.bytes);
    }

    let mut interpreter = generated_cpu(mode);
    let mut clif = generated_cpu(mode);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut clif_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    for case in &cases {
        restore_bus(&mut interpreter_bus, &pristine);
        restore_bus(&mut clif_bus, &pristine);
        arm(&mut interpreter, case);
        run_to_halt(&mut interpreter, &mut interpreter_bus, case).unwrap();
        prime_clif(&mut clif, &mut clif_bus, &pristine, case);

        restore_bus(&mut interpreter_bus, &pristine);
        restore_bus(&mut clif_bus, &pristine);
        arm(&mut interpreter, case);
        arm(&mut clif, case);
        let before = clif.jit_clif_counters();
        let expected_fpu = interpreter.fpu.clone();

        let interpreted = run_to_halt(&mut interpreter, &mut interpreter_bus, case);
        let native = run_to_halt(&mut clif, &mut clif_bus, case);

        assert_eq!(native, interpreted, "run outcome differs: {case:#?}");
        assert_eq!(clif.registers, interpreter.registers, "{case:#?}");
        assert_eq!(clif.registers.eip, interpreter.registers.eip, "{case:#?}");
        assert_eq!(clif.eflags(), interpreter.eflags(), "{case:#?}");
        assert_eq!(clif.fpu, expected_fpu, "clif x87 changed: {case:#?}");
        assert_eq!(
            interpreter.fpu, expected_fpu,
            "interpreter x87 changed: {case:#?}"
        );
        assert_eq!(clif, interpreter, "full CPU state differs: {case:#?}");
        assert_eq!(clif_bus.memory, interpreter_bus.memory, "{case:#?}");
        assert_eq!(clif.elapsed_clocks, interpreter.elapsed_clocks, "{case:#?}");
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interpreter_bus.trace.elapsed_clocks(),
            "bus clocks differ: {case:#?}"
        );
        assert!(
            clif.jit_clif_counters().entries > before.entries,
            "accepted seed never entered the clif shell: {case:#?}, clif={:#?}, perf={:#?}",
            clif.jit_clif_counters(),
            clif.perf_counters()
        );
    }
}

#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn generated_clif_shells_match_interpreter_in_486_and_586_modes() {
    run_generated_mode_clif(GswMode::Gsw486, 0);
    run_generated_mode_clif(GswMode::Gsw586, CASES_PER_MODE);
}

fn single_case_memory(case: &GeneratedCase) -> Vec<u8> {
    let mut memory = vec![0; MEMORY_LEN];
    let start = case.entry as usize;
    memory[start..start + case.bytes.len()].copy_from_slice(&case.bytes);
    memory
}

fn assert_measured_pair(
    interpreter: &mut CpuGsw,
    interpreter_bus: &mut TestBus,
    direct: &mut CpuGsw,
    direct_bus: &mut TestBus,
    pristine: &[u8],
    case: &GeneratedCase,
    exact_run_boundaries: bool,
) -> u64 {
    restore_bus(interpreter_bus, pristine);
    restore_bus(direct_bus, pristine);
    arm(interpreter, case);
    arm(direct, case);
    let direct_before = direct.perf_counters().jit_direct_insns;
    let interpreted = run_to_halt(interpreter, interpreter_bus, case);
    let native = run_to_halt(direct, direct_bus, case);
    if exact_run_boundaries {
        assert_eq!(native, interpreted, "{case:#?}");
    } else {
        let interpreted_clocks: u64 = interpreted
            .as_ref()
            .expect("fallback case must halt")
            .iter()
            .map(|outcome| u64::from(outcome.consumed_core_clocks))
            .sum();
        let native_clocks: u64 = native
            .as_ref()
            .expect("fallback case must halt")
            .iter()
            .map(|outcome| u64::from(outcome.consumed_core_clocks))
            .sum();
        assert_eq!(
            native_clocks, interpreted_clocks,
            "{case:#?}, native={native:?}, interpreted={interpreted:?}, native_elapsed={}, \
             interpreted_elapsed={}",
            direct.elapsed_clocks, interpreter.elapsed_clocks
        );
    }
    assert_eq!(direct, interpreter, "{case:#?}");
    assert_eq!(direct.eflags(), interpreter.eflags(), "{case:#?}");
    assert_eq!(direct_bus.memory, interpreter_bus.memory, "{case:#?}");
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{case:#?}"
    );
    direct
        .perf_counters()
        .jit_direct_insns
        .saturating_sub(direct_before)
}

#[test]
fn generated_block_rebuilds_after_live_mode_change_and_honors_interrupt_shadow() {
    let case = generated_case(7, 0x100);
    let pristine = single_case_memory(&case);
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
    assert!(
        assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &case,
            true,
        ) > 0
    );

    restore_bus(&mut interpreter_bus, &pristine);
    restore_bus(&mut direct_bus, &pristine);
    arm(&mut interpreter, &case);
    arm(&mut direct, &case);
    interpreter.interrupt_shadow = true;
    direct.interrupt_shadow = true;
    let before = direct.perf_counters().jit_direct_insns;
    let interpreted = run_to_halt(&mut interpreter, &mut interpreter_bus, &case);
    let native = run_to_halt(&mut direct, &mut direct_bus, &case);
    assert_eq!(native, interpreted, "{case:#?}");
    assert_eq!(direct, interpreter, "{case:#?}");
    assert_eq!(direct_bus.memory, interpreter_bus.memory, "{case:#?}");
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{case:#?}"
    );
    assert!(direct.perf_counters().jit_direct_insns > before);

    interpreter.set_mode(GswMode::Gsw586);
    direct.set_mode(GswMode::Gsw586);
    assert_eq!(
        direct.jit_direct.len(),
        0,
        "mode change retained native code"
    );
    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
    assert!(
        assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &case,
            true,
        ) > 0
    );
}

#[test]
fn generated_paged_blocks_match_with_wp_set_and_supervisor_override() {
    for (wp, writable) in [(true, true), (false, false)] {
        let case = generated_case(9, u32::from(wp) * 0x200);
        let mut pristine = single_case_memory(&case);
        pristine[0x3000..0x3004].copy_from_slice(&0x4003u32.to_le_bytes());
        for page in 0..32u32 {
            let flags = if page == 0x10 && !writable { 1 } else { 3 };
            let pte = (page << 12) | flags;
            let offset = 0x4000 + page as usize * 4;
            pristine[offset..offset + 4].copy_from_slice(&pte.to_le_bytes());
        }

        let mut interpreter = generated_cpu(GswMode::Gsw486);
        let mut direct = generated_cpu(GswMode::Gsw486);
        for cpu in [&mut interpreter, &mut direct] {
            cpu.control.cr0 |= CR0_PG;
            if wp {
                cpu.control.cr0 |= CR0_WP;
            } else {
                cpu.control.cr0 &= !CR0_WP;
            }
            cpu.control.cr3 = 0x3000;
        }
        let mut interpreter_bus = TestBus::with_memory(pristine.clone());
        let mut direct_bus = TestBus::with_memory(pristine.clone());
        for bus in [&mut interpreter_bus, &mut direct_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }

        restore_bus(&mut interpreter_bus, &pristine);
        arm(&mut interpreter, &case);
        run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
        prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
        assert!(
            assert_measured_pair(
                &mut interpreter,
                &mut interpreter_bus,
                &mut direct,
                &mut direct_bus,
                &pristine,
                &case,
                true,
            ) > 0,
            "paged block did not retire natively: wp={wp}, writable={writable}"
        );
    }
}

fn paging_alias_case() -> GeneratedCase {
    let mut bytes = vec![0x90, 0xa1];
    push_u32(&mut bytes, 0x1_0100);
    bytes.extend_from_slice(&[0x8b, 0x1d]);
    push_u32(&mut bytes, 0x1_1100);
    bytes.extend_from_slice(&[0x03, 0xc3, 0xa3]);
    push_u32(&mut bytes, 0x1_0104);
    bytes.extend_from_slice(&[0x89, 0x1d]);
    push_u32(&mut bytes, 0x1_1108);
    bytes.extend_from_slice(&[0x85, 0xc0, 0x75, 1, 0xf4, 0xf4]);
    let mut gpr = [0; 8];
    gpr[4] = 0x1_f000;
    GeneratedCase {
        seed: 0xa11a_5000_0000_0001,
        entry: 0x1000,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 256,
    }
}

#[test]
fn generated_paging_aliases_share_one_native_physical_page() {
    let case = paging_alias_case();
    let mut pristine = single_case_memory(&case);
    pristine[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
    for page in 0..32u32 {
        let pte = (page << 12) | 7;
        let offset = 0x4000 + page as usize * 4;
        pristine[offset..offset + 4].copy_from_slice(&pte.to_le_bytes());
    }
    pristine[0x4040..0x4044].copy_from_slice(&0x6007u32.to_le_bytes());
    pristine[0x4044..0x4048].copy_from_slice(&0x6007u32.to_le_bytes());
    pristine[0x6100..0x6104].copy_from_slice(&5u32.to_le_bytes());

    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    for cpu in [&mut interpreter, &mut direct] {
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
    }
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
    let exits = direct.perf_counters().jit_direct_side_exits;
    assert!(
        assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &case,
            true,
        ) > 0
    );
    assert_eq!(direct.perf_counters().jit_direct_side_exits, exits);
    assert_eq!(
        &direct_bus.memory[0x6104..0x610c],
        &[10, 0, 0, 0, 5, 0, 0, 0]
    );
}

fn linked_successor_case() -> GeneratedCase {
    let mut bytes = vec![0x90, 0xb8];
    push_u32(&mut bytes, 1);
    bytes.extend_from_slice(&[
        0x83, 0xc0, 1, // add eax,1
        0x85, 0xc0, // test eax,eax
        0x75, 1,    // jnz memory successor
        0xf4, // fallthrough stop
        0xa1,
    ]);
    push_u32(&mut bytes, 0x1_0000);
    bytes.extend_from_slice(&[
        0x89, 0xc1, // mov ecx,eax
        0x85, 0xc9, // test ecx,ecx
        0x75, 1,    // jnz register successor
        0xf4, // fallthrough stop
        0x83, 0xc1, 2, // add ecx,2
        0x89, 0xca, // mov edx,ecx
        0x85, 0xd2, // test edx,edx
        0x75, 1, // jnz stop
        0xf4, 0xf4,
    ]);
    let mut gpr = [0; 8];
    gpr[4] = 0x1_f000;
    GeneratedCase {
        seed: 0x11ab_1e00_0000_0001,
        entry: 0x1000,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 4096,
    }
}

#[test]
fn generated_three_block_chain_aggregates_across_event_caps() {
    let mut case = linked_successor_case();
    let mut pristine = single_case_memory(&case);
    pristine[0x1_0000..0x1_0004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
    interpreter_bus.uniform_native_fetches = true;
    direct_bus.uniform_native_fetches = true;
    let linked_before = direct.perf_counters().jit_direct_linked_transfers;
    let mut native = 0;
    for cap in [1, 7, 31, 127, 511, 4096] {
        case.cap = cap;
        native += assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &case,
            true,
        );
    }
    assert!(native > 0, "event-cap sweep retired no native instructions");
    assert!(
        direct.perf_counters().jit_direct_linked_transfers >= linked_before + 2,
        "three-block chain never linked both successors: {case:#?}, perf={:#?}",
        direct.perf_counters()
    );
}

fn unaligned_cross_page_case() -> GeneratedCase {
    let mut bytes = vec![0x90, 0xa1];
    push_u32(&mut bytes, 0x1_8000);
    bytes.extend_from_slice(&[0x8b, 0x0d]);
    push_u32(&mut bytes, 0x1_8001);
    bytes.extend_from_slice(&[0x8b, 0x15]);
    push_u32(&mut bytes, 0x1_8fff);
    bytes.extend_from_slice(&[
        0x03, 0xc1, // add eax,ecx
        0x03, 0xc2, // add eax,edx
        0x85, 0xc0, // test eax,eax
        0x75, 1, 0xf4, 0xf4,
    ]);
    let mut gpr = [0; 8];
    gpr[4] = 0x1_f000;
    GeneratedCase {
        seed: 0xa119_ed00_c205_5001,
        entry: 0x1000,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 256,
    }
}

#[test]
fn generated_unaligned_and_cross_page_dwords_take_precise_native_exits() {
    let case = unaligned_cross_page_case();
    let mut pristine = single_case_memory(&case);
    for (offset, value) in [(0x1_8000, 1u32), (0x1_8fff, 0x1020_3040)] {
        pristine[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &case);
    let exits = direct
        .perf_counters()
        .jit_direct_exit_cross_page_or_alignment;
    assert!(
        assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &case,
            false,
        ) > 0
    );
    assert!(
        direct
            .perf_counters()
            .jit_direct_exit_cross_page_or_alignment
            >= exits + 2,
        "both dword fallbacks must exit before access: {case:#?}, perf={:#?}",
        direct.perf_counters()
    );
}

fn faulting_case() -> GeneratedCase {
    let mut bytes = vec![0x90, 0xb8];
    push_u32(&mut bytes, 1);
    bytes.push(0xbb);
    push_u32(&mut bytes, 2);
    bytes.push(0xa1);
    push_u32(&mut bytes, 0x1_0000);
    bytes.push(0xa1);
    push_u32(&mut bytes, 0x3_0000);
    bytes.extend_from_slice(&[0x89, 0xc1, 0x85, 0xc9, 0x75, 1, 0xf4, 0xf4]);
    let mut gpr = [0; 8];
    gpr[4] = 0x1_f000;
    GeneratedCase {
        seed: 0xfa17_ed00_0000_0001,
        entry: 0x1000,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 256,
    }
}

fn run_to_error(
    cpu: &mut CpuGsw,
    bus: &mut TestBus,
    case: &GeneratedCase,
) -> (Vec<BudgetedRunOutcome>, CpuError) {
    let mut outcomes = Vec::new();
    for _ in 0..64 {
        match cpu.run_budgeted(bus, case.cap) {
            Ok(outcome) => outcomes.push(outcome),
            Err(error) => return (outcomes, error),
        }
    }
    panic!("generated fault case did not raise an error: {case:#?}")
}

#[test]
fn generated_native_prefix_preserves_fault_outcome_and_charged_clocks() {
    let case = faulting_case();
    let mut pristine = single_case_memory(&case);
    pristine[0x3000..0x3004].copy_from_slice(&0x4003u32.to_le_bytes());
    for page in 0..32u32 {
        let pte = (page << 12) | 3;
        let offset = 0x4000 + page as usize * 4;
        pristine[offset..offset + 4].copy_from_slice(&pte.to_le_bytes());
    }
    pristine[0x1_0000..0x1_0004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    for cpu in [&mut interpreter, &mut direct] {
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
    }
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &case);
    run_to_error(&mut interpreter, &mut interpreter_bus, &case);
    direct.set_jit_auto_admit(false);
    restore_bus(&mut direct_bus, &pristine);
    arm(&mut direct, &case);
    run_to_error(&mut direct, &mut direct_bus, &case);
    direct.set_jit_auto_admit(true);
    for _ in 0..4 {
        restore_bus(&mut direct_bus, &pristine);
        arm(&mut direct, &case);
        run_to_error(&mut direct, &mut direct_bus, &case);
    }
    interpreter_bus.uniform_native_fetches = true;
    direct_bus.uniform_native_fetches = true;

    restore_bus(&mut interpreter_bus, &pristine);
    restore_bus(&mut direct_bus, &pristine);
    arm(&mut interpreter, &case);
    arm(&mut direct, &case);
    let native_before = direct.perf_counters().jit_direct_insns;
    let interpreted = run_to_error(&mut interpreter, &mut interpreter_bus, &case);
    let native = run_to_error(&mut direct, &mut direct_bus, &case);
    assert_eq!(native, interpreted, "{case:#?}");
    assert_eq!(direct, interpreter, "{case:#?}");
    assert_eq!(direct_bus.memory, interpreter_bus.memory, "{case:#?}");
    assert_eq!(
        direct_bus.trace.elapsed_clocks(),
        interpreter_bus.trace.elapsed_clocks(),
        "{case:#?}"
    );
    assert!(direct.perf_counters().jit_direct_insns > native_before);
}

fn watched_store_case(value: u32, target: u32) -> GeneratedCase {
    let entry = 0x1000;
    let mut bytes = vec![0x90, 0xb8];
    push_u32(&mut bytes, 1);
    bytes.extend_from_slice(&[0xb9]);
    push_u32(&mut bytes, 2);
    bytes.extend_from_slice(&[0x89, 0x17]);
    bytes.extend_from_slice(&[0x89, 0xc3, 0x85, 0xdb, 0x75, 1, 0xf4, 0xf4]);
    let mut gpr = [0; 8];
    gpr[2] = value;
    gpr[4] = 0x1_f000;
    gpr[7] = target;
    GeneratedCase {
        seed: 0x5a4d_4300_0000_0000 | u64::from(value),
        entry,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 256,
    }
}

#[test]
fn generated_watched_store_exits_for_same_and_changed_code() {
    let prime = watched_store_case(1, 0x1080);
    let same = watched_store_case(1, 0x1080);
    let changed = watched_store_case(2, 0x1080);
    let mut pristine = single_case_memory(&prime);
    pristine[0x1080..0x1084].copy_from_slice(&1u32.to_le_bytes());
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    let mut interpreter_bus = TestBus::with_memory(pristine.clone());
    let mut direct_bus = TestBus::with_memory(pristine.clone());
    for bus in [&mut interpreter_bus, &mut direct_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    restore_bus(&mut interpreter_bus, &pristine);
    arm(&mut interpreter, &prime);
    run_to_halt(&mut interpreter, &mut interpreter_bus, &prime).unwrap();
    prime_direct(&mut direct, &mut direct_bus, &pristine, &prime);
    let rejected = jit::direct::BlockKey::new(0x1080, 0x1080, direct.jit_mode_key());
    assert!(matches!(
        direct.jit_direct.probe(rejected),
        jit::direct::BlockProbe::Interpret
    ));
    assert!(matches!(
        direct.jit_direct.probe(rejected),
        jit::direct::BlockProbe::Compile
    ));
    direct
        .jit_direct
        .reject(jit::direct::RejectedSpan::new(rejected, 4).expect("page-local rejected fixture"));

    let exits = direct.perf_counters().jit_direct_exit_code_watch;
    assert!(
        assert_measured_pair(
            &mut interpreter,
            &mut interpreter_bus,
            &mut direct,
            &mut direct_bus,
            &pristine,
            &same,
            true,
        ) > 0
    );
    assert_eq!(direct.perf_counters().jit_direct_exit_code_watch, exits + 1);
    // G2: the same-value store side-exits the native block (the watch fires) but elides the
    // invalidation, so the rejected span survives and admission does not churn. The probe stays
    // Rejected with no re-reject needed; only a value-changing store re-opens the region.
    assert!(matches!(
        direct.jit_direct.probe(rejected),
        jit::direct::BlockProbe::Rejected
    ));

    let native = assert_measured_pair(
        &mut interpreter,
        &mut interpreter_bus,
        &mut direct,
        &mut direct_bus,
        &pristine,
        &changed,
        true,
    );
    assert!(native > 0, "changed watched store lost its native prefix");
    assert_eq!(direct.perf_counters().jit_direct_exit_code_watch, exits + 2);
    assert!(matches!(
        direct.jit_direct.probe(rejected),
        jit::direct::BlockProbe::Interpret
    ));
}

struct A20Bus {
    inner: TestBus,
    enabled: bool,
}

impl A20Bus {
    fn map(&self, address: u32) -> u32 {
        if self.enabled {
            address
        } else {
            address & !(1 << 20)
        }
    }
}

impl CpuBus for A20Bus {
    fn native_aggregate_accounting_allowed(&self) -> bool {
        true
    }

    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let mapped = self.map(address);
        self.inner.read_memory(mapped, width, kind)
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        let mapped = self.map(address);
        self.inner.write_memory(mapped, width, value, kind)
    }

    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        if !self.enabled && address & (1 << 20) != 0 {
            return Ok(None);
        }
        let requested_page = address & !0x0fff;
        let mapped_page = self.map(requested_page);
        let Some(mut page) = self.inner.direct_page(mapped_page, kind)? else {
            return Ok(None);
        };
        page.physical_page = requested_page;
        Ok(Some(page))
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let mapped = self.map(address);
        self.inner.prefetch_memory(mapped, out)
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        let mapped = self.map(address);
        self.inner.charge_instruction_fetch(mapped)
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        self.inner
            .read_io(port, width, core_clocks_so_far, cpu_is_ring0_pm)
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.inner
            .write_io(port, width, value, core_clocks_so_far, cpu_is_ring0_pm)
    }

    fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError> {
        self.inner.interrupt_acknowledge(vector, ax)
    }
}

fn restore_a20_bus(bus: &mut A20Bus, pristine: &[u8]) {
    bus.inner.memory.copy_from_slice(pristine);
    bus.inner.trace.clear();
}

fn prime_a20_direct(cpu: &mut CpuGsw, bus: &mut A20Bus, pristine: &[u8], case: &GeneratedCase) {
    cpu.set_jit_auto_admit(false);
    restore_a20_bus(bus, pristine);
    arm(cpu, case);
    run_to_halt(cpu, bus, case).unwrap();
    cpu.set_jit_auto_admit(true);
    for _ in 0..4 {
        restore_a20_bus(bus, pristine);
        arm(cpu, case);
        run_to_halt(cpu, bus, case).unwrap();
    }
}

fn a20_case() -> GeneratedCase {
    let mut bytes = vec![0x90, 0xb8];
    push_u32(&mut bytes, 0);
    bytes.push(0xa1);
    push_u32(&mut bytes, 0x300);
    bytes.push(0xa1);
    push_u32(&mut bytes, 0x10_0300);
    bytes.extend_from_slice(&[0x89, 0xc1, 0x85, 0xc9, 0x75, 1, 0xf4, 0xf4]);
    let mut gpr = [0; 8];
    gpr[4] = 0x1_f000;
    GeneratedCase {
        seed: 0xa20a_11a5_0000_0001,
        entry: 0x1000,
        bytes,
        gpr,
        eflags: 0x202,
        cap: 128,
    }
}

#[test]
fn generated_hma_load_tracks_a20_alias_and_cache_invalidation() {
    let case = a20_case();
    let mut pristine = vec![0; 0x10_2000];
    let start = case.entry as usize;
    pristine[start..start + case.bytes.len()].copy_from_slice(&case.bytes);
    pristine[0x300..0x304].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    pristine[0x10_0300..0x10_0304].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());
    let mut interpreter = generated_cpu(GswMode::Gsw486);
    let mut direct = generated_cpu(GswMode::Gsw486);
    let mut interpreter_bus = A20Bus {
        inner: TestBus::with_memory(pristine.clone()),
        enabled: false,
    };
    let mut direct_bus = A20Bus {
        inner: TestBus::with_memory(pristine.clone()),
        enabled: false,
    };
    interpreter_bus.inner.direct_pages_enabled = true;
    direct_bus.inner.direct_pages_enabled = true;

    for expected in [0x1122_3344, 0xaabb_ccdd] {
        let a20_enabled = direct_bus.enabled;
        restore_a20_bus(&mut interpreter_bus, &pristine);
        arm(&mut interpreter, &case);
        run_to_halt(&mut interpreter, &mut interpreter_bus, &case).unwrap();
        prime_a20_direct(&mut direct, &mut direct_bus, &pristine, &case);

        restore_a20_bus(&mut interpreter_bus, &pristine);
        restore_a20_bus(&mut direct_bus, &pristine);
        arm(&mut interpreter, &case);
        arm(&mut direct, &case);
        let before = direct.perf_counters().jit_direct_insns;
        let unavailable = direct.perf_counters().jit_direct_exit_unavailable_or_kind;
        let interpreted = run_to_halt(&mut interpreter, &mut interpreter_bus, &case);
        let native = run_to_halt(&mut direct, &mut direct_bus, &case);
        assert_eq!(native, interpreted, "{case:#?}");
        assert_eq!(direct, interpreter, "{case:#?}");
        assert_eq!(direct_bus.inner.memory, interpreter_bus.inner.memory);
        assert_eq!(
            direct_bus.inner.trace.elapsed_clocks(),
            interpreter_bus.inner.trace.elapsed_clocks()
        );
        assert_eq!(direct.registers.eax(), expected);
        assert_eq!(direct.registers.ecx(), expected);
        assert!(direct.perf_counters().jit_direct_insns > before);
        let new_unavailable = direct.perf_counters().jit_direct_exit_unavailable_or_kind;
        if a20_enabled {
            assert_eq!(new_unavailable, unavailable);
        } else {
            assert!(new_unavailable > unavailable);
        }

        interpreter_bus.enabled = true;
        direct_bus.enabled = true;
        interpreter.note_a20_changed();
        direct.note_a20_changed();
        assert_eq!(
            direct.jit_direct.len(),
            0,
            "A20 change retained native code"
        );
    }
}
