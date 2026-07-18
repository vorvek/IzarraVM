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

/// The clif differential arm: the same generator suite routed through the clif policy
/// instead of Direct. Since C1b the units retire their leading register/immediate runs
/// NATIVELY (real lowering), yet state and timing must stay BYTE-IDENTICAL to the plain
/// interpreter run, including the pending-flag descriptor bytes, raw eflags, elapsed core
/// clocks, and bus trace clocks. A mismatch indicts the lowering, the guard layer, or the
/// batch charging.
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

// ---------------------------------------------------------------------------------------
// Track C C1b: the forced-case clif lowering battery (design section 6.1). Each case is a
// small guest program routed through the interpreter and the clif policy, asserting
// BYTE-IDENTICAL full CPU state (registers, raw eflags, the pending descriptor itself,
// x87), memory, and the section 5.3 timing set (elapsed core clocks, bus trace clocks,
// timing_rem, fp_rem) after every pass, including the compile pass. The random generator
// cannot be trusted to hit the ADC/SBB carry-set arms, the shift count classes, or the
// word partial-write merges at useful density, so they are forced here explicitly.
// ---------------------------------------------------------------------------------------

#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn assert_clif_forced_case(mode: GswMode, code: &[u8], gpr: [u32; 8], eflags: u32) {
    let _ = run_clif_forced_case(mode, code, gpr, eflags);
}

/// The forced-case harness body, returning the clif CPU so C1c's side-exit-reason cases can
/// additionally assert the diagnostic counters after the byte-identity checks pass.
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn run_clif_forced_case(
    mode: GswMode,
    code: &[u8],
    gpr: [u32; 8],
    eflags: u32,
) -> CpuGsw {
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    let entry = 0x1000u32;
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);

    let mut interp = generated_cpu(mode);
    let mut clif = generated_cpu(mode);
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }

    for pass in 0..6 {
        for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
            cpu.halted = false;
            cpu.interrupt_shadow = false;
            cpu.registers.gpr = gpr;
            cpu.registers.eflags = eflags;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            cpu.fp_rem = 0;
            cpu.fpu.finit();
            cpu.fpu.push(1.25);
            cpu.fpu.push(-2.5);
            for _ in 0..64 {
                let outcome = cpu
                    .run_budgeted(bus, 4096)
                    .expect("forced case runs to halt");
                if outcome.halted {
                    break;
                }
            }
            assert!(cpu.halted, "forced case did not halt: {code:02x?}");
        }
        assert_eq!(clif.registers, interp.registers, "pass {pass}: {code:02x?}");
        assert_eq!(
            clif.pending_flags, interp.pending_flags,
            "pending descriptor differs, pass {pass}: {code:02x?}"
        );
        assert_eq!(
            clif.registers.eflags, interp.registers.eflags,
            "raw eflags differ, pass {pass}: {code:02x?}"
        );
        assert_eq!(clif.eflags(), interp.eflags(), "pass {pass}: {code:02x?}");
        assert_eq!(clif.fpu, interp.fpu, "pass {pass}: {code:02x?}");
        assert_eq!(clif, interp, "full state, pass {pass}: {code:02x?}");
        assert_eq!(
            clif_bus.memory, interp_bus.memory,
            "pass {pass}: {code:02x?}"
        );
        assert_eq!(
            clif.elapsed_clocks, interp.elapsed_clocks,
            "core clocks, pass {pass}: {code:02x?}"
        );
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "bus clocks, pass {pass}: {code:02x?}"
        );
        assert_eq!(
            clif.timing_rem, interp.timing_rem,
            "timing remainder, pass {pass}: {code:02x?}"
        );
        assert_eq!(
            clif.fp_rem, interp.fp_rem,
            "fp remainder, pass {pass}: {code:02x?}"
        );
    }
    assert!(
        clif.jit_clif_counters().entries > 0,
        "forced case never entered a clif unit: {code:02x?}, {:#?}",
        clif.jit_clif_counters()
    );
    clif
}

#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
pub(super) fn forced_gpr() -> [u32; 8] {
    // ESP parked in scratch RAM; values chosen to exercise carries, sign bits, and byte
    // lanes (AH/DH nonzero high lanes).
    [
        0x8000_00ff,
        0x0000_0011,
        0xfff0_1234,
        0x7fff_ffff,
        0x1_f000,
        0x0f0f_0f0f,
        0xdead_beef,
        0x0000_0000,
    ]
}

/// ADC/SBB, both carry arms, dword and byte operand forms (design section 3.3): the
/// carry-clear arm must leave the LAZY descriptor, the carry-set arm the EAGER eflags.
/// Entry carry comes from an interpreter-retired STC/CLC (the unit's entry-state read) and
/// from an in-unit producer (the SSA-resident read).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_adc_sbb_both_carry_arms() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // STC; ADC EAX,EBX (carry set, entry state); HLT
        assert_clif_forced_case(mode, &[0xf9, 0x11, 0xd8, 0xf4], forced_gpr(), 0x202);
        // CLC; ADC EAX,EBX (carry clear, entry state; lazy arm); HLT
        assert_clif_forced_case(mode, &[0xf8, 0x11, 0xd8, 0xf4], forced_gpr(), 0x203);
        // STC; SBB ECX,EDX; HLT
        assert_clif_forced_case(mode, &[0xf9, 0x19, 0xd1, 0xf4], forced_gpr(), 0x202);
        // CLC; SBB ECX,EDX; HLT
        assert_clif_forced_case(mode, &[0xf8, 0x19, 0xd1, 0xf4], forced_gpr(), 0x203);
        // In-unit producer: ADD EAX,EBX (sets carry from 0x800000ff + 0x7fffffff);
        // ADC EDX,ECX; SBB EAX,EBX; HLT
        assert_clif_forced_case(
            mode,
            &[0x01, 0xd8, 0x11, 0xca, 0x19, 0xd8, 0xf4],
            forced_gpr(),
            0x202,
        );
        // Byte forms through 0x80 /2 and /3 (ADC/SBB AL/CH,imm8), both entry-carry arms.
        assert_clif_forced_case(mode, &[0xf9, 0x80, 0xd0, 0x7f, 0xf4], forced_gpr(), 0x202);
        assert_clif_forced_case(mode, &[0xf8, 0x80, 0xd0, 0x7f, 0xf4], forced_gpr(), 0x202);
        assert_clif_forced_case(mode, &[0xf9, 0x80, 0xdd, 0x81, 0xf4], forced_gpr(), 0x202);
        // ADC through the dword immediate group 0x81 /2 and the sign-extended 0x83 /3.
        assert_clif_forced_case(
            mode,
            &[0xf9, 0x81, 0xd3, 0xff, 0xff, 0xff, 0x7f, 0xf4],
            forced_gpr(),
            0x202,
        );
        assert_clif_forced_case(mode, &[0xf9, 0x83, 0xdb, 0x80, 0xf4], forced_gpr(), 0x202);
    }
}

/// INC/DEC preserve CF through cf_override, from both a live entry CF and an in-unit
/// pending producer; the descriptor bytes themselves are compared (b == 1, tag bits
/// 16/17).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_inc_dec_preserve_cf() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // STC; INC EAX; HLT and CLC; DEC EBX; HLT (entry-state CF).
        assert_clif_forced_case(mode, &[0xf9, 0x40, 0xf4], forced_gpr(), 0x202);
        assert_clif_forced_case(mode, &[0xf8, 0x4b, 0xf4], forced_gpr(), 0x203);
        // ADD sets CF; INC must carry it through the pending override; then DEC again.
        assert_clif_forced_case(mode, &[0x01, 0xd8, 0x41, 0x4a, 0xf4], forced_gpr(), 0x202);
        // SUB sets borrow; DEC and INC of wrapping values.
        assert_clif_forced_case(mode, &[0x29, 0xd9, 0x4f, 0x47, 0xf4], forced_gpr(), 0x202);
    }
}

/// Single shifts: count classes 0, 1, > 1, 31, and a masked-to-zero 32, for SHL/SHR/SAR
/// (immediate counts; the single-shift CL form is not in the admitted set), plus the
/// implicit-count 0xd1 forms. Counts follow an in-unit flag producer AND stand alone at
/// unit entry (the runtime pending-none arm of set_shift_result_flags).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_shift_count_classes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for op in [0xe0u8, 0xe8, 0xf8] {
            for count in [0u8, 1, 5, 31, 32] {
                // TEST EAX,EBX first: the shift sees a live Logic descriptor.
                assert_clif_forced_case(
                    mode,
                    &[0x85, 0xd8, 0xc1, op, count, 0xf4],
                    forced_gpr(),
                    0x202,
                );
                // Shift at unit entry (behind an interpreted NOP starter, so the unit
                // begins AT the shift): the shift sees pending none at runtime and the
                // stale descriptor bytes must survive untouched.
                assert_clif_forced_case(mode, &[0x90, 0xc1, op, count, 0xf4], forced_gpr(), 0x2d7);
            }
            // 0xd1: the implicit count-1 encoding, after an ADD producer.
            assert_clif_forced_case(mode, &[0x01, 0xd8, 0xd1, op, 0xf4], forced_gpr(), 0x202);
        }
    }
}

/// Double shifts (SHLD/SHRD, dword): immediate counts 0/1/>1/31 and CL counts, with the
/// flag state both live-pending and none at the shift.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_double_shift_count_classes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for count in [0u8, 1, 7, 31] {
            // SHLD EAX,EBX,imm and SHRD EDX,ESI,imm after ADD (pending live).
            assert_clif_forced_case(
                mode,
                &[0x01, 0xd8, 0x0f, 0xa4, 0xd8, count, 0xf4],
                forced_gpr(),
                0x202,
            );
            assert_clif_forced_case(
                mode,
                &[0x01, 0xd8, 0x0f, 0xac, 0xf2, count, 0xf4],
                forced_gpr(),
                0x202,
            );
            // At unit entry behind an interpreted NOP starter (pending none at the
            // shift).
            assert_clif_forced_case(
                mode,
                &[0x90, 0x0f, 0xa4, 0xd8, count, 0xf4],
                forced_gpr(),
                0x2d7,
            );
            // CL forms: MOV CL,count; SHLD by CL then SHRD by CL.
            assert_clif_forced_case(
                mode,
                &[0xb1, count, 0x0f, 0xa5, 0xd8, 0x0f, 0xad, 0xf2, 0xf4],
                forced_gpr(),
                0x202,
            );
        }
    }
}

/// The no-flags group: register/immediate moves (including the AH/CH/DH high byte lanes),
/// LEA with base/index/scale/displacement, and the TEST immediate forms.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_moves_lea_and_test_forms() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        assert_clif_forced_case(
            mode,
            &[
                0xb8, 0x78, 0x56, 0x34, 0x12, // mov eax,0x12345678
                0xb4, 0xa5, // mov ah,0xa5
                0xb1, 0x5a, // mov cl,0x5a
                0x88, 0xe6, // mov dh,ah
                0x88, 0xc8, // mov al,cl
                0x89, 0xc7, // mov edi,eax
                0x8d, 0x84, 0x8e, 0x40, 0x02, 0x00, 0x00, // lea eax,[esi+ecx*4+0x240]
                0xc7, 0xc3, 0x11, 0x22, 0x33, 0x44, // mov ebx,0x44332211 (c7 /0 reg)
                0xc6, 0xc5, 0x99, // mov ch,0x99 (c6 /0 reg)
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
        // TEST forms: 0x85, 0xa8 (AL,imm8), 0xa9 (EAX,imm32), 0xf6 /0, 0xf7 /0.
        assert_clif_forced_case(
            mode,
            &[
                0x85, 0xf7, // test edi,esi
                0xa8, 0x81, // test al,0x81
                0xa9, 0x00, 0x00, 0x00, 0x80, // test eax,0x80000000
                0xf6, 0xc6, 0xff, // test dh,0xff
                0xf7, 0xc2, 0x34, 0x12, 0x00, 0x00, // test edx,0x1234
                0xf4,
            ],
            forced_gpr(),
            0x2d7,
        );
    }
}

/// I586 word forms: MOV r16 partial writes (high half preserved), word INC/DEC, and the
/// word CMPs (0x39/0x3b), the exact word whitelist the classifier admits.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_word_forms_on_586() {
    let code = [
        0x66, 0x89, 0xd8, // mov ax,bx (high EAX half must survive)
        0x66, 0x40, // inc ax
        0x66, 0x4a, // dec dx
        0x66, 0x39, 0xc8, // cmp ax,cx
        0x66, 0x3b, 0xf7, // cmp si,di
        0xf4,
    ];
    assert_clif_forced_case(GswMode::Gsw586, &code, forced_gpr(), 0x202);
    // Word INC wrap at 0xffff with CF preservation from STC.
    assert_clif_forced_case(
        GswMode::Gsw586,
        &[0xf9, 0x66, 0x40, 0xf4],
        [0x1234_ffff, 0, 0, 0, 0x1_f000, 0, 0, 0],
        0x202,
    );
}

/// The Logic-tag group's live-AF write (alu_logic): AND/OR/XOR register and immediate
/// forms, with a live AF inherited from a pending producer and from raw eflags.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_logic_af_semantics() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // ADD (sets pending with AF-worthy operands); AND: AF must materialize live.
        assert_clif_forced_case(mode, &[0x01, 0xd8, 0x21, 0xca, 0xf4], forced_gpr(), 0x202);
        // Entry AF set in raw eflags, pending none: OR then XOR then the imm forms keep it.
        assert_clif_forced_case(
            mode,
            &[
                0x09, 0xd8, 0x31, 0xf7, 0x83, 0xc9, 0x0f, 0x80, 0xe2, 0x3c, 0xf4,
            ],
            forced_gpr(),
            0x212,
        );
    }
}

// ---------------------------------------------------------------------------------------
// Track C C1b: the x87 call-out differential battery (design section 6.1's call-out group,
// the end-to-end twin of clif/callout_proof_test.rs): real CpuGsw/CpuBus through the
// widened ABI, all three dispositions.
// ---------------------------------------------------------------------------------------

/// Continue path: lowered integer instructions mixed with x87 call-outs in one unit. The
/// shim delegates each x87 instruction to the interpreter (charging fp clocks through the
/// normal path) and the unit resumes its next lowered slot; state, x87 stack, fp_rem, and
/// all clocks must stay byte-identical (the F1 mixed-mechanism charging shape: batch static
/// profile for the lowered slots, interpreter path for the call-outs, in one entry).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_x87_callout_continue_path() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // add eax,ebx; fld1; fadd st0,st1; inc ecx; fmulp; dec edx; hlt
        let code = [
            0x01, 0xd8, // add eax,ebx
            0xd9, 0xe8, // fld1
            0xd8, 0xc1, // fadd st0,st1
            0x41, // inc ecx
            0xde, 0xc9, // fmulp st1,st0
            0x4a, // dec edx
            0xf4,
        ];
        assert_clif_forced_case(mode, &code, forced_gpr(), 0x202);
    }
}

/// Exit path (design finding B1's end-to-end pin): the x87 instruction delivers a real
/// architectural fault (#NM under CR0.TS) through the interpreter inside the call-out; the
/// retire is Ok with EIP redirected to the handler, the shim's fall-through predicate
/// catches it, and the unit exits WITHOUT running its next lowered slot; the handler EIP is
/// preserved bit-for-bit. State equality against the interpreter covers all of it (the
/// interpreter also never runs the slot after the faulting instruction).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_x87_callout_fault_exit_path() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        let entry = 0x1000u32;
        // add eax,ebx; fld1 (#NM under TS); inc ecx (must never run); hlt
        let code = [0x01, 0xd8, 0xd9, 0xe8, 0x41, 0xf4];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);
        // #NM handler: hlt at 0x2000. Interrupt gate for vector 7 in an IDT at 0x3000.
        let handler = 0x2000u32;
        memory[handler as usize] = 0xf4;
        let idt = 0x3000usize;
        let gate = idt + 7 * 8;
        memory[gate] = (handler & 0xff) as u8;
        memory[gate + 1] = ((handler >> 8) & 0xff) as u8;
        memory[gate + 2] = 0x08; // selector low
        memory[gate + 3] = 0x00; // selector high
        memory[gate + 4] = 0x00;
        memory[gate + 5] = 0x8e; // present 32-bit interrupt gate
        memory[gate + 6] = ((handler >> 16) & 0xff) as u8;
        memory[gate + 7] = ((handler >> 24) & 0xff) as u8;
        // Flat GDT at 0x3800 (code 0x08, data 0x10) so the gate's CS selector loads.
        let gdt = 0x3800usize;
        memory[gdt + 8..gdt + 16].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9b, 0xcf, 0]);
        memory[gdt + 16..gdt + 24].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x93, 0xcf, 0]);

        let mut interp = generated_cpu(mode);
        let mut clif = generated_cpu(mode);
        clif.set_clif_backend_enabled(true);
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }

        for pass in 0..6 {
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                cpu.registers.gpr = forced_gpr();
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.control.cr0 |= CR0_TS;
                cpu.idtr = DescriptorTable {
                    base: 0x3000,
                    limit: 0x7ff,
                };
                cpu.gdtr = DescriptorTable {
                    base: 0x3800,
                    limit: 0xff,
                };
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                cpu.fp_rem = 0;
                for _ in 0..64 {
                    let outcome = cpu.run_budgeted(bus, 4096).expect("fault case reaches hlt");
                    if outcome.halted {
                        break;
                    }
                }
                assert!(cpu.halted, "fault case did not halt");
            }
            assert_eq!(clif.registers, interp.registers, "pass {pass}");
            assert_eq!(
                clif.registers.eip,
                handler.wrapping_add(1),
                "both policies must halt inside the #NM handler, pass {pass}"
            );
            assert_eq!(clif.pending_flags, interp.pending_flags, "pass {pass}");
            assert_eq!(clif, interp, "pass {pass}");
            assert_eq!(clif_bus.memory, interp_bus.memory, "pass {pass}");
            assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "pass {pass}");
            assert_eq!(
                clif_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "pass {pass}"
            );
            assert_eq!(clif.timing_rem, interp.timing_rem, "pass {pass}");
            assert_eq!(clif.fp_rem, interp.fp_rem, "pass {pass}");
        }
        assert!(
            clif.jit_clif_counters().entries > 0,
            "fault case never entered a clif unit: {:#?}",
            clif.jit_clif_counters()
        );
    }
}

/// Hard-stop path (design finding B2's end-to-end pin): a genuine bus error arriving from
/// INSIDE the call-out (an x87 memory operand at an unmapped address) must relay through
/// CLIF_CALLOUT_HARD_STOP as the IDENTICAL Err(CpuError) the interpreter-only policy
/// returns from the same guest program, with identical CPU state left behind.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_x87_callout_bus_error_hard_stop() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        let entry = 0x1000u32;
        // add eax,ebx; fld dword [0x00ff0000] (unmapped in TestBus); inc ecx; hlt
        let code = [
            0x01, 0xd8, // add eax,ebx
            0xd9, 0x05, 0x00, 0x00, 0xff, 0x00, // fld dword [0x00ff0000]
            0x41, // inc ecx (must never run)
            0xf4,
        ];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);

        let mut interp = generated_cpu(mode);
        let mut clif = generated_cpu(mode);
        clif.set_clif_backend_enabled(true);
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }

        for pass in 0..6 {
            let mut errors = Vec::new();
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                cpu.registers.gpr = forced_gpr();
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                cpu.fp_rem = 0;
                cpu.fpu.finit();
                let mut error = None;
                for _ in 0..64 {
                    match cpu.run_budgeted(bus, 4096) {
                        Ok(outcome) => {
                            assert!(!outcome.halted, "the guest must error before hlt");
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
                errors.push(error.expect("the unmapped x87 load must produce a bus error"));
            }
            assert_eq!(
                errors[0], errors[1],
                "pass {pass}: relayed Err must be identical"
            );
            assert_eq!(clif.registers, interp.registers, "pass {pass}");
            assert_eq!(clif.pending_flags, interp.pending_flags, "pass {pass}");
            assert_eq!(clif, interp, "pass {pass}");
            assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "pass {pass}");
            assert_eq!(clif.timing_rem, interp.timing_rem, "pass {pass}");
            assert_eq!(clif.fp_rem, interp.fp_rem, "pass {pass}");
        }
        assert!(
            clif.jit_clif_counters().entries > 0,
            "hard-stop case never entered a clif unit: {:#?}",
            clif.jit_clif_counters()
        );
    }
}

/// Review finding B1's reproducer, pinned: an x87 call-out that stores into the IN-FLIGHT
/// unit's own remaining guest bytes must exit the unit rather than run stale lowering. The
/// warm passes store to scratch (the Continue path); the final pass points EBX at the
/// unit's own `inc eax` byte and stores an f32 whose bits decode as four `inc ebx`. The
/// interpreter re-fetches and executes the fresh bytes (eax stays 0, ebx walks to 0x1007);
/// the clif policy must match, which requires the invalidation-generation exit latch (the
/// SMC choke alone only kills the unit for the NEXT entry).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_x87_store_into_own_unit_exits_in_flight() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        let entry = 0x1000u32;
        // nop (interpreted starter); fstp dword [ebx]; inc eax; hlt
        let code = [0x90, 0xd9, 0x1b, 0x40, 0xf4];
        let inc_eax_addr = entry + 3;
        // f32 whose stored bits are 43 43 43 43: four `inc ebx` over the inc eax byte and
        // the trailing hlt bytes (the fill after them is hlt again).
        let smc_value = f64::from(f32::from_bits(0x4343_4343));

        let mut memory = vec![0xf4u8; MEMORY_LEN];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);

        let mut interp = generated_cpu(mode);
        let mut clif = generated_cpu(mode);
        clif.set_clif_backend_enabled(true);
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }

        for pass in 0..6 {
            let final_pass = pass == 5;
            let ebx = if final_pass { inc_eax_addr } else { 0x5000 };
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                cpu.halted = false;
                cpu.interrupt_shadow = false;
                cpu.registers.gpr = [0, 0, 0, ebx, 0x1_f000, 0, 0, 0];
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                cpu.fp_rem = 0;
                cpu.fpu.finit();
                cpu.fpu.push(smc_value);
                for _ in 0..64 {
                    let outcome = cpu.run_budgeted(bus, 4096).expect("smc case reaches hlt");
                    if outcome.halted {
                        break;
                    }
                }
                assert!(cpu.halted, "smc case did not halt, pass {pass}");
            }
            assert_eq!(clif.registers, interp.registers, "pass {pass}");
            assert_eq!(clif.pending_flags, interp.pending_flags, "pass {pass}");
            assert_eq!(clif, interp, "pass {pass}");
            assert_eq!(clif_bus.memory, interp_bus.memory, "pass {pass}");
            assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "pass {pass}");
            assert_eq!(
                clif_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "pass {pass}"
            );
            assert_eq!(clif.timing_rem, interp.timing_rem, "pass {pass}");
            if final_pass {
                // The fresh bytes ran: the overwritten inc eax never executed, and the four
                // inc ebx did.
                assert_eq!(clif.registers.eax(), 0, "stale lowering executed inc eax");
                assert_eq!(clif.registers.ebx(), inc_eax_addr + 4);
            }
        }
        assert!(
            clif.jit_clif_counters().entries > 0,
            "smc case never entered a clif unit: {:#?}",
            clif.jit_clif_counters()
        );
    }
}

/// M1: the compile-outcome clif arm. A walkable unit whose ENTRY slot is not lowerable
/// (a memory form) parks Dormant instead of compiling a no-op body; a compiled unit killed
/// by an SMC write is dropped from the cache entirely (generation bumped) and the next
/// encounter re-admits through the normal Seen -> Compiled path.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_compile_outcomes_dormant_and_smc_readmission() {
    use crate::jit::clif::cache::{ClifUnitState, clif_key_for};

    let entry = 0x1000u32;
    // nop; jmp +0; hlt: a TERMINAL at the unit entry is classifiable but never lowered
    // (terminals are C1d's job), so the unit at it has leading 0. Earlier fixtures used a
    // Load and then an RmwIncDec here; C1c's increments made every memory form lowerable.
    let load_code = [0x90, 0xeb, 0x00, 0xf4];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + load_code.len()].copy_from_slice(&load_code);
    let mut cpu = generated_cpu(GswMode::Gsw586);
    cpu.set_clif_backend_enabled(true);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    for _ in 0..4 {
        cpu.halted = false;
        cpu.registers.gpr = [0, 0, 0, 0, 0x1_f000, 0, 0, 0];
        cpu.set_eip(entry);
        while !cpu.run_budgeted(&mut bus, 4096).expect("runs").halted {}
    }
    let key = clif_key_for(&cpu, entry + 1, true).expect("warm key");
    assert_eq!(
        cpu.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Dormant),
        "a leading-0 unit must park Dormant"
    );
    assert_eq!(cpu.jit_clif_counters().units_installed, 0);

    // Fresh CPU: a real unit compiles, an SMC write into its span drops the entry and
    // bumps the generation, and the next encounters re-admit (Seen, then Compiled again).
    let code = [0x90, 0x01, 0xd8, 0x40, 0xf4];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);
    let mut cpu = generated_cpu(GswMode::Gsw586);
    cpu.set_clif_backend_enabled(true);
    let mut bus = TestBus::with_memory(memory);
    bus.direct_pages_enabled = true;
    bus.direct_page_clocks = true;
    let run_pass = |cpu: &mut CpuGsw, bus: &mut TestBus| {
        cpu.halted = false;
        cpu.registers.gpr = [1, 0, 0, 2, 0x1_f000, 0, 0, 0];
        cpu.registers.eflags = 0x202;
        cpu.set_eip(entry);
        while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
    };
    for _ in 0..4 {
        run_pass(&mut cpu, &mut bus);
    }
    let key = clif_key_for(&cpu, entry + 1, true).expect("warm key");
    assert!(matches!(
        cpu.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
    assert!(cpu.jit_clif_counters().units_installed >= 1);
    let generation = cpu.jit_direct.clif_units.generation;

    // SMC: rewrite the add's modrm byte (same-length instruction, different registers).
    cpu.write_memory_u8(
        &mut bus,
        SegmentIndex::Ds,
        entry + 2,
        0xda,
        BusAccessKind::DataWrite,
    )
    .expect("smc store");
    assert!(
        cpu.jit_direct.clif_units.state(key).is_none(),
        "the killed unit's entry must drop entirely"
    );
    assert!(
        cpu.jit_direct.clif_units.generation > generation,
        "the invalidation generation must move"
    );

    for _ in 0..4 {
        run_pass(&mut cpu, &mut bus);
    }
    assert!(matches!(
        cpu.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
    assert!(
        cpu.jit_clif_counters().units_installed >= 2,
        "the rewritten span must recompile through the normal admission path: {:#?}",
        cpu.jit_clif_counters()
    );
}

// ---------------------------------------------------------------------------------------
// Track C C1c increment 1: the Load/Store memory-path battery (design section 5, the
// subset for the two lowered variants). Every case asserts byte-identical state, memory,
// and the section 5.3 timing set against the interpreter; the side-exit cases additionally
// pin the diagnostic reason counters and the invalidation behavior.
// ---------------------------------------------------------------------------------------

/// Load/Store width and addressing matrix: moffs and modrm displacement forms, byte lanes
/// (AH/CH high lanes), base and base+index*scale+disp addressing, and the store-immediate
/// forms whose slot carries BOTH an operand immediate and a displacement (the operand
/// table's two-lane case). Word forms run on the 586 persona only (the walker's
/// operand-size-prefix gate mirrors Direct's compile heuristic).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_load_store_width_matrix() {
    let mut gpr = forced_gpr();
    gpr[3] = 0x5100; // EBX: modrm base register
    gpr[6] = 0x5200; // ESI: sib base
    gpr[2] = 4; // EDX: sib index
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // moffs dword load/store (0xa1/0xa3), byte load/store through high lanes
        // (mov ah,[disp] / mov [disp],ch), then a dword load of the stored bytes.
        assert_clif_forced_case(
            mode,
            &[
                0xa1, 0x00, 0x50, 0x00, 0x00, // mov eax, [0x5000]
                0xa3, 0x04, 0x50, 0x00, 0x00, // mov [0x5004], eax
                0x8a, 0x25, 0x08, 0x50, 0x00, 0x00, // mov ah, [0x5008]
                0x88, 0x2d, 0x0c, 0x50, 0x00, 0x00, // mov [0x500c], ch
                0x8b, 0x1d, 0x04, 0x50, 0x00, 0x00, // mov ebx, [0x5004]
                0xf4,
            ],
            gpr,
            0x202,
        );
        // Register addressing: base+disp8 and base+index*scale+disp8, load then store.
        assert_clif_forced_case(
            mode,
            &[
                0x8b, 0x43, 0x10, // mov eax, [ebx+0x10]
                0x89, 0x4c, 0x96, 0x20, // mov [esi+edx*4+0x20], ecx
                0x8a, 0x44, 0x96, 0x20, // mov al, [esi+edx*4+0x20]
                0xf4,
            ],
            gpr,
            0x202,
        );
        // Store-immediate forms: one slot holding an operand immediate AND a displacement
        // (the two-lane operand table), dword and byte.
        assert_clif_forced_case(
            mode,
            &[
                0xc7, 0x05, 0x10, 0x50, 0x00, 0x00, 0x44, 0x33, 0x22,
                0x11, // mov dword [0x5010], 0x11223344
                0xc6, 0x05, 0x14, 0x50, 0x00, 0x00, 0x5a, // mov byte [0x5014], 0x5a
                0xa1, 0x10, 0x50, 0x00, 0x00, // mov eax, [0x5010] (reads the store back)
                0xf4,
            ],
            gpr,
            0x202,
        );
    }
    // Word forms (586 persona only; a word form is exactly one operand-size override).
    assert_clif_forced_case(
        GswMode::Gsw586,
        &[
            0x66, 0x8b, 0x0d, 0x18, 0x50, 0x00, 0x00, // mov cx, [0x5018]
            0x66, 0x89, 0x35, 0x1c, 0x50, 0x00, 0x00, // mov [0x501c], si
            0xf4,
        ],
        gpr,
        0x202,
    );
}

/// The wide-page guard's two reject conditions (design section 2.6): a misaligned dword
/// and a page-straddling dword both side-exit with the alignment reason and identical
/// final state; the LAST non-straddling aligned address (page offset 0xffc, exactly
/// `0x1000 - width`) retires natively with no alignment exit, pinning the `>` versus `>=`
/// boundary math.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_wide_guard_misaligned_straddle_and_boundary() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // The leading aligned load exists so at least one canonical access populates the
        // FastMap (creating its storage; an all-rejecting unit would otherwise never see
        // native_bases and never compile, on either backend).
        let clif = run_clif_forced_case(
            mode,
            &[
                0xa1, 0x00, 0x50, 0x00, 0x00, // mov eax, [0x5000]: aligned, populates
                0xa1, 0x01, 0x50, 0x00, 0x00, // mov eax, [0x5001]: misaligned
                0xa1, 0xfe, 0x5f, 0x00, 0x00, // mov eax, [0x5ffe]: straddles into 0x6000
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
        assert!(
            clif.jit_clif_counters().mem_exit_alignment > 0,
            "wide-guard cases must exit with the alignment reason: {:#?}",
            clif.jit_clif_counters()
        );

        let clif = run_clif_forced_case(
            mode,
            &[
                0x40, // inc eax
                0xa1, 0xfc, 0x5f, 0x00, 0x00, // mov eax, [0x5ffc]: last non-straddling dword
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
        assert_eq!(
            clif.jit_clif_counters().mem_exit_alignment,
            0,
            "page offset 0xffc must NOT reject (the design's > 0x1000 - width, not >=): {:#?}",
            clif.jit_clif_counters()
        );
    }
}

/// The UNAVAILABLE_BIAS sentinel path (design section 2.3, the epoch mechanism's actual
/// enforcement point): invalidating the FastMap under a compiled unit makes its next entry
/// side-exit with the unavailable reason; the interpreter performs the canonical access
/// and repopulates, so a later entry retires natively again. State stays byte-identical to
/// the interpreter throughout.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_unavailable_page_side_exits_then_recovers() {
    let code: &[u8] = &[
        0x40, // inc eax
        0xa1, 0x00, 0x50, 0x00, 0x00, // mov eax, [0x5000]
        0xa3, 0x04, 0x50, 0x00, 0x00, // mov [0x5004], eax
        0xf4,
    ];
    let entry = 0x1000u32;
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);

    let mut interp = generated_cpu(GswMode::Gsw586);
    let mut clif = generated_cpu(GswMode::Gsw586);
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let pass = |interp: &mut CpuGsw,
                clif: &mut CpuGsw,
                interp_bus: &mut TestBus,
                clif_bus: &mut TestBus| {
        for (cpu, bus) in [
            (&mut *interp, &mut *interp_bus),
            (&mut *clif, &mut *clif_bus),
        ] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = forced_gpr();
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
        }
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(clif.timing_rem, interp.timing_rem);
    };
    for _ in 0..4 {
        pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    }
    let before = clif.jit_clif_counters();
    assert!(before.entries > 0, "unit never entered: {before:#?}");

    // Drop every FastMap mapping: the unit's next entry must side-exit on the sentinel.
    clif.jit_fast_map.invalidate_all();
    pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    let after = clif.jit_clif_counters();
    assert!(
        after.mem_exit_unavailable_or_kind > before.mem_exit_unavailable_or_kind,
        "invalidated map must exit with the unavailable reason: {after:#?}"
    );

    // The interpreter's canonical re-execution repopulated; the next pass is native again
    // with no further unavailable exits.
    pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    let recovered = clif.jit_clif_counters();
    assert_eq!(
        recovered.mem_exit_unavailable_or_kind, after.mem_exit_unavailable_or_kind,
        "repopulated pages must stop exiting: {recovered:#?}"
    );
    assert!(recovered.entries > after.entries);
}

/// Segment-limit checks (design section 1.1): a finite limit that admits the access emits
/// the compare and retires natively; an access past the limit side-exits and the
/// interpreter's canonical re-execution raises the same fault the interpreter-only policy
/// would; a limit smaller than the access width takes the compile-time UNCONDITIONAL exit
/// (the m3 underflow edge).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_segment_limit_pass_fail_and_underflow() {
    let entry = 0x1000u32;
    let finite_ds = |limit: u32| SegmentRegister {
        selector: 0x10,
        base: 0,
        limit,
        access: 0x93,
        default_size_32: true,
    };
    // (DS limit, code, expect_error). Each faulting case carries a leading IN-LIMIT access
    // so at least one canonical access populates the FastMap (no storage, no native_bases,
    // no compile, on either backend); the underflow case populates through an ES override
    // (nothing fits under a DS limit of 1), which also stops unit growth before it, so the
    // underflow load compiles as its own single-slot unit.
    let cases: [(u32, Vec<u8>, bool); 3] = [
        // Finite admitted limit: the compare is emitted and passes, native retire.
        (
            0x1_ffff,
            vec![0x40, 0xa1, 0x00, 0x50, 0x00, 0x00, 0xf4],
            false,
        ),
        // In-limit load at 0x4000 (max_start 0x4ffe admits it), then the over-limit load
        // at 0x5000: side exit, interpreter faults.
        (
            0x5001,
            vec![
                0x40, 0xa1, 0x00, 0x40, 0x00, 0x00, 0xa1, 0x00, 0x50, 0x00, 0x00, 0xf4,
            ],
            true,
        ),
        // Underflow: DS limit 1 admits NO dword anywhere, the compile-time unconditional
        // exit; the ES-override load populates and is retired by the interpreter.
        (
            0x0001,
            vec![
                0x40, 0x26, 0xa1, 0x00, 0x40, 0x00, 0x00, 0xa1, 0x00, 0x00, 0x00, 0x00, 0xf4,
            ],
            true,
        ),
    ];
    for (limit, code, expect_error) in cases {
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(&code);

        let mut interp = generated_cpu(GswMode::Gsw586);
        let mut clif = generated_cpu(GswMode::Gsw586);
        clif.set_clif_backend_enabled(true);
        for cpu in [&mut interp, &mut clif] {
            cpu.registers
                .set_segment(SegmentIndex::Ds, finite_ds(limit));
        }
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        for _ in 0..4 {
            let mut results = Vec::new();
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                bus.memory.copy_from_slice(&memory);
                bus.trace.clear();
                cpu.halted = false;
                cpu.registers.gpr = forced_gpr();
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                let mut outcome = Ok(());
                for _ in 0..64 {
                    match cpu.run_budgeted(bus, 4096) {
                        Ok(o) if o.halted => break,
                        Ok(_) => {}
                        Err(error) => {
                            outcome = Err(error);
                            break;
                        }
                    }
                }
                results.push(outcome);
            }
            assert_eq!(
                results[0], results[1],
                "run outcome differs at limit {limit:#x}"
            );
            assert_eq!(results[1].is_err(), expect_error, "limit {limit:#x}");
            assert_eq!(clif.registers, interp.registers, "limit {limit:#x}");
            assert_eq!(clif.eflags(), interp.eflags(), "limit {limit:#x}");
            assert_eq!(clif_bus.memory, interp_bus.memory, "limit {limit:#x}");
            assert_eq!(
                clif.elapsed_clocks, interp.elapsed_clocks,
                "limit {limit:#x}"
            );
            assert_eq!(
                clif_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "limit {limit:#x}"
            );
        }
        let counters = clif.jit_clif_counters();
        assert!(counters.entries > 0, "limit {limit:#x}: {counters:#?}");
        if expect_error {
            assert!(
                counters.mem_exit_segment_limit > 0,
                "limit {limit:#x} must exit with the segment-limit reason: {counters:#?}"
            );
        } else {
            assert_eq!(
                counters.mem_exit_segment_limit, 0,
                "admitted access must pass the emitted limit check: {counters:#?}"
            );
        }
    }
}

/// The extended B1 probe (design section 5 test 3): a unit whose OWN lowered store targets
/// its own resident code bytes. The inline code-watch check (NOT the generation latch,
/// which only covers call-out returns) side-exits before the store commits; the
/// interpreter re-executes canonically. A same-value store survives through G2 elision
/// (the unit stays compiled); a value-changing store invalidates the unit through
/// note_code_write_hit, dropping its cache entry and releasing its watch registration.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_lowered_store_into_own_code_side_exits_before_commit() {
    use crate::jit::clif::cache::{ClifUnitState, clif_key_for};

    let entry = 0x1000u32;
    // nop (interpreted starter); mov [0x1004],eax (an ALIGNED store into the unit's OWN
    // bytes: 0x1004 holds the mov's high displacement bytes, then inc ebx, then hlt);
    // inc ebx; hlt. The unit spans 0x1001..0x1008. Alignment matters: a misaligned target
    // would exit on the wide guard first and never reach the code-watch check.
    let code: &[u8] = &[0x90, 0xa3, 0x04, 0x10, 0x00, 0x00, 0x43, 0xf4];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    // The little-endian dword currently at 0x1004 (disp high bytes, inc ebx, hlt):
    // storing it back is the SAME-VALUE case (G2 elides the invalidation).
    let same_value = u32::from_le_bytes([0x00, 0x00, 0x43, 0xf4]);
    // One opcode byte differs (inc ebx becomes inc ecx): a REAL code change, still a
    // valid program for the rest of the pass on both arms.
    let changed_value = u32::from_le_bytes([0x00, 0x00, 0x41, 0xf4]);

    let mut interp = generated_cpu(GswMode::Gsw586);
    let mut clif = generated_cpu(GswMode::Gsw586);
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let pass = |interp: &mut CpuGsw,
                clif: &mut CpuGsw,
                interp_bus: &mut TestBus,
                clif_bus: &mut TestBus,
                eax: u32| {
        for (cpu, bus) in [
            (&mut *interp, &mut *interp_bus),
            (&mut *clif, &mut *clif_bus),
        ] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = [eax, 0, 0, 0, 0x1_f000, 0, 0, 0];
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
        }
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
    };

    // Prime with the same-value store so the unit compiles and survives.
    for _ in 0..4 {
        pass(
            &mut interp,
            &mut clif,
            &mut interp_bus,
            &mut clif_bus,
            same_value,
        );
    }
    let key = clif_key_for(&clif, entry + 1, true).expect("warm key");
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
    // M5's registration probe: the installed unit's own bytes read watched through the
    // SHARED NativeCodeWatch immediately after install.
    assert!(
        clif.jit_direct.range_hits_compiled_code(entry + 1, 7),
        "the installed unit's own range must be watch-registered"
    );

    // Same-value pass: the inline check fires BEFORE the commit (the counter moves), the
    // interpreter's canonical store elides invalidation (G2), and the unit survives.
    let before = clif.jit_clif_counters();
    pass(
        &mut interp,
        &mut clif,
        &mut interp_bus,
        &mut clif_bus,
        same_value,
    );
    let after = clif.jit_clif_counters();
    assert!(
        after.mem_exit_code_watch > before.mem_exit_code_watch,
        "the own-code store must exit through the code-watch check: {after:#?}"
    );
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));

    // Changed-value pass: the check still exits before the commit; the interpreter's real
    // write invalidates the unit and drops its watch registration.
    let generation = clif.jit_direct.clif_units.generation;
    pass(
        &mut interp,
        &mut clif,
        &mut interp_bus,
        &mut clif_bus,
        changed_value,
    );
    let final_counters = clif.jit_clif_counters();
    assert!(
        final_counters.mem_exit_code_watch > after.mem_exit_code_watch,
        "the changed-value store must also exit through the check: {final_counters:#?}"
    );
    assert!(
        clif.jit_direct.clif_units.state(key).is_none(),
        "the self-modified unit must drop from the cache"
    );
    assert!(clif.jit_direct.clif_units.generation > generation);
    assert!(
        !clif.jit_direct.range_hits_compiled_code(entry + 1, 7),
        "the dropped unit's watch registration must release"
    );
}

/// The m5 chunk-granularity companion: a store into the unit's own PAGE but an unwatched
/// 16-byte CHUNK passes both watch tables and retires natively with identical final state.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_same_page_unwatched_chunk_store_retires_natively() {
    // nop; mov [0x1800],eax (same 4KB page as the unit, chunk 0x180 vs the unit's 0x100);
    // inc ebx; hlt.
    let clif = run_clif_forced_case(
        GswMode::Gsw586,
        &[0x90, 0xa3, 0x00, 0x18, 0x00, 0x00, 0x43, 0xf4],
        forced_gpr(),
        0x202,
    );
    let counters = clif.jit_clif_counters();
    assert_eq!(
        counters.mem_exit_code_watch, 0,
        "an unwatched chunk in a watched page must not exit: {counters:#?}"
    );
}

/// CPL3 permission checks (design section 2.4), both directions: a ring-3 read of a
/// supervisor page and a ring-3 write of a read-only user page side-exit with the
/// permission reason and defer to the interpreter's canonical outcome; a ring-3 access to
/// a user read-write page retires natively THROUGH the emitted permission check.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_cpl3_permission_side_exits() {
    let entry = 0x1000u32;
    let ring3_cpu = || {
        let mut cpu = generated_cpu(GswMode::Gsw586);
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0b,
                base: 0,
                limit: u32::MAX,
                access: 0xfb,
                default_size_32: true,
            },
        );
        for segment in [
            SegmentIndex::Ds,
            SegmentIndex::Ss,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            cpu.registers.set_segment(
                segment,
                SegmentRegister {
                    selector: 0x13,
                    base: 0,
                    limit: u32::MAX,
                    access: 0xf3,
                    default_size_32: true,
                },
            );
        }
        cpu.cpl = 3;
        cpu
    };
    // Page tables: PDE at 0x3000 -> user table at 0x4000; every page user RW (flags 7)
    // except page 0x10 supervisor (flags 3) and page 0x11 user read-only (flags 5).
    let build_memory = |code: &[u8]| {
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
        memory[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
        for page in 0..32u32 {
            let flags = match page {
                0x10 => 3,
                0x11 => 5,
                _ => 7,
            };
            let pte = (page << 12) | flags;
            let offset = 0x4000 + page as usize * 4;
            memory[offset..offset + 4].copy_from_slice(&pte.to_le_bytes());
        }
        memory
    };
    // A clif permission exit needs the target page POPULATED (flags present) but lacking
    // the required permission bit: an unpopulated page exits earlier through the kind
    // check. The read-only-write case populates its own page through the (permitted)
    // ring-3 read of the same address; the supervisor-read case needs a ring-0
    // pre-population pass (a ring-3 access to a supervisor page can never populate it),
    // run with clif DISABLED so no ring-0 unit squats on the key before the CPL3 unit
    // compiles.
    let supervisor_read: &[u8] = &[
        0x40, 0xa1, 0x00, 0x20, 0x01, 0x00, 0xa1, 0x00, 0x00, 0x01, 0x00, 0xf4,
    ];
    let readonly_write: &[u8] = &[
        0x40, 0xa1, 0x00, 0x10, 0x01, 0x00, 0xa3, 0x00, 0x10, 0x01, 0x00, 0xf4,
    ];
    let user_rw: &[u8] = &[0x40, 0xa1, 0x00, 0x20, 0x01, 0x00, 0xf4];
    for (code, expect_permission_exit, ring0_prepopulate) in [
        (supervisor_read, true, true),
        (readonly_write, true, false),
        (user_rw, false, false),
    ] {
        let memory = build_memory(code);
        let mut interp = ring3_cpu();
        let mut clif = ring3_cpu();
        clif.set_clif_backend_enabled(true);
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        if ring0_prepopulate {
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                cpu.cpl = 0;
                cpu.registers
                    .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x08, 0x9b));
                for segment in [
                    SegmentIndex::Ds,
                    SegmentIndex::Ss,
                    SegmentIndex::Es,
                    SegmentIndex::Fs,
                    SegmentIndex::Gs,
                ] {
                    cpu.registers
                        .set_segment(segment, SegmentRegister::flat(0x10, 0x93));
                }
                bus.memory.copy_from_slice(&memory);
                bus.trace.clear();
                cpu.halted = false;
                cpu.registers.gpr = [0x1122_3344, 0, 0, 0, 0x1_f000, 0, 0, 0];
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                while !cpu.run_budgeted(bus, 4096).expect("ring0 populate").halted {}
                // Back to ring 3 for the measured passes.
                cpu.cpl = 3;
                cpu.registers.set_segment(
                    SegmentIndex::Cs,
                    SegmentRegister {
                        selector: 0x0b,
                        base: 0,
                        limit: u32::MAX,
                        access: 0xfb,
                        default_size_32: true,
                    },
                );
                for segment in [
                    SegmentIndex::Ds,
                    SegmentIndex::Ss,
                    SegmentIndex::Es,
                    SegmentIndex::Fs,
                    SegmentIndex::Gs,
                ] {
                    cpu.registers.set_segment(
                        segment,
                        SegmentRegister {
                            selector: 0x13,
                            base: 0,
                            limit: u32::MAX,
                            access: 0xf3,
                            default_size_32: true,
                        },
                    );
                }
                // Drop any ring-0 unit squatting on the key (the mode key carries no
                // CPL), so the ring-3 passes compile a fresh memory_cpl3 unit. The
                // FastMap population from the ring-0 pass survives; only the units go.
                cpu.jit_direct.clif_clear();
            }
        }
        for _ in 0..4 {
            let mut results = Vec::new();
            for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
                bus.memory.copy_from_slice(&memory);
                bus.trace.clear();
                cpu.halted = false;
                cpu.registers.gpr = [0x1122_3344, 0, 0, 0, 0x1_f000, 0, 0, 0];
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                let mut outcome = Ok(());
                for _ in 0..64 {
                    match cpu.run_budgeted(bus, 4096) {
                        Ok(o) if o.halted => break,
                        Ok(_) => {}
                        Err(error) => {
                            outcome = Err(error);
                            break;
                        }
                    }
                }
                results.push(outcome);
            }
            assert_eq!(results[0], results[1], "outcome differs: {code:02x?}");
            assert_eq!(clif.registers, interp.registers, "{code:02x?}");
            assert_eq!(clif.eflags(), interp.eflags(), "{code:02x?}");
            assert_eq!(clif_bus.memory, interp_bus.memory, "{code:02x?}");
            assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "{code:02x?}");
            assert_eq!(
                clif_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "{code:02x?}"
            );
        }
        let counters = clif.jit_clif_counters();
        assert!(counters.entries > 0, "{code:02x?}: {counters:#?}");
        if expect_permission_exit {
            assert!(
                counters.mem_exit_permission > 0,
                "{code:02x?} must exit with the permission reason: {counters:#?}"
            );
        } else {
            assert_eq!(
                counters.mem_exit_permission, 0,
                "{code:02x?} must pass the emitted permission check: {counters:#?}"
            );
        }
    }
}

/// M4's access-count pin for the increment-1 variants: clif's `plan_unit` counts over a
/// walked layout equal the counts Direct's own compile loop accumulates over the SAME
/// guest bytes, by shared accessor code (the full nine-variant corpus including the CMP
/// AluMemDest and byte AluMemSource asymmetries lands with those variants' increments).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_plan_access_counts_match_direct_compilation() {
    use crate::jit::clif::cache::walk_unit;
    use crate::jit::clif::lower::plan_unit;

    let entry = 0x1000u32;
    // Two corpora covering every increment-1 access shape (dword/byte/word loads and
    // stores, moffs and modrm, store-immediate), each small enough that Direct's compile
    // heuristics admit every slot, so the two backends cover IDENTICAL slot lists and the
    // count comparison is like-for-like.
    let corpora: [&[u8]; 8] = [
        &[
            0x40, // inc eax
            0xa1, 0x00, 0x50, 0x00, 0x00, // mov eax, [0x5000]     (dword read)
            0x8a, 0x1d, 0x04, 0x50, 0x00, 0x00, // mov bl, [0x5004] (byte read)
            0x66, 0x8b, 0x0d, 0x06, 0x50, 0x00, 0x00, // mov cx, [0x5006] (word read)
            0xa3, 0x08, 0x50, 0x00, 0x00, // mov [0x5008], eax     (dword store)
            0xf4,
        ],
        &[
            0x40, // inc eax
            0x66, 0x89, 0x15, 0x0c, 0x50, 0x00, 0x00, // mov [0x500c], dx (word store)
            0x88, 0x1d, 0x0e, 0x50, 0x00, 0x00, // mov [0x500e], bl (byte store)
            0xc7, 0x05, 0x10, 0x50, 0x00, 0x00, 0x44, 0x33, 0x22, 0x11, // mov [0x5010], imm
            0xf4,
        ],
        // C1c increment 2: Push counts a dword store, Pop a dword read, by the same
        // shared accessors (four slots from entry+1, above Direct's three-slot minimum).
        &[
            0x40, // inc eax (the interpreted-starter position; both walks begin after it)
            0x50, // push eax (dword store)
            0x53, // push ebx (dword store)
            0x59, // pop ecx (dword read)
            0x5a, // pop edx (dword read)
            0xf4,
        ],
        // C1c increment 3: AluMemSource reads (word/dword; the byte form does not exist
        // by classification, its zero-contribution pin lives in
        // clif_byte_alu_mem_source_asymmetry; the word form exists only as CMP per the
        // classifier's word gate).
        &[
            0x40, // inc eax
            0x03, 0x05, 0x00, 0x50, 0x00, 0x00, // add eax, [0x5000] (dword read)
            0x66, 0x3b, 0x05, 0x04, 0x50, 0x00, 0x00, // cmp ax, [0x5004] (word read)
            0x2b, 0x1d, 0x08, 0x50, 0x00, 0x00, // sub ebx, [0x5008] (dword read)
            0xf4,
        ],
        // TestImmMem byte and dword reads (memory_alu slots, kept within Direct's
        // four-instruction memory-alu block cap).
        &[
            0x40, // inc eax
            0xf6, 0x05, 0x0c, 0x50, 0x00, 0x00, 0x7f, // test byte [0x500c], 0x7f
            0xf7, 0x05, 0x10, 0x50, 0x00, 0x00, 0x44, 0x33, 0x22,
            0x11, // test dword [0x5010], 0x11223344
            0x43, // inc ebx
            0xf4,
        ],
        // C1c increment 4: AluMemDest, including the second M4 asymmetry (CMP counts a
        // READ at its width but NO store, the op 0..=6 gate on the store accessors).
        &[
            0x40, // inc eax
            0x01, 0x1d, 0x00, 0x50, 0x00, 0x00, // add [0x5000], ebx (read + store)
            0x39, 0x0d, 0x04, 0x50, 0x00, 0x00, // cmp [0x5004], ecx (read, NO store)
            0x80, 0x3d, 0x08, 0x50, 0x00, 0x00, 0x5a, // cmp byte [0x5008], 0x5a (byte read)
            0xf4,
        ],
        // C1c increment 5: RmwIncDec counts a read AND a store at its width (word and
        // dword; the byte-lane pin lives in clif_byte_rmw_inc_dec_non_existence).
        &[
            0x40, // inc eax
            0xff, 0x05, 0x00, 0x50, 0x00, 0x00, // inc dword [0x5000]
            0x66, 0xff, 0x0d, 0x04, 0x50, 0x00, 0x00, // dec word [0x5004]
            0x43, // inc ebx
            0xf4,
        ],
        // DoubleShiftMem: one dword read plus one dword store per slot (memory_alu, kept
        // within Direct's block cap).
        &[
            0x40, // inc eax
            0x0f, 0xa4, 0x1d, 0x00, 0x50, 0x00, 0x00, 0x07, // shld [0x5000], ebx, 7
            0x0f, 0xad, 0x0d, 0x04, 0x50, 0x00, 0x00, // shrd [0x5004], ecx, cl
            0x43, // inc ebx
            0xf4,
        ],
    ];
    for code in corpora {
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
        let mut cpu = generated_cpu(GswMode::Gsw586);
        cpu.jit_direct.set_fast_map_enabled_for_test(true);
        let mut bus = TestBus::with_memory(memory);
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
        cpu.registers.gpr = [0, 0, 0, 0, 0x1_f000, 0, 0, 0];
        cpu.set_eip(entry);
        while !cpu.run_budgeted(&mut bus, 4096).expect("runs").halted {}

        let layout = walk_unit(&cpu, entry + 1, true).expect("walked layout");
        let plan = plan_unit(&layout.kinds, true);
        assert_eq!(
            usize::from(plan.leading),
            layout.kinds.len(),
            "every slot in this corpus must lower: {code:02x?}"
        );
        let compilation = match crate::jit::direct::compile(&mut cpu, entry + 1, true) {
            crate::jit::direct::CompileOutcome::Compiled(compilation) => compilation,
            crate::jit::direct::CompileOutcome::StructuralReject(_) => {
                panic!("direct structurally rejected: {code:02x?}")
            }
            crate::jit::direct::CompileOutcome::Retry => {
                panic!("direct retried: {code:02x?}")
            }
        };
        assert_eq!(
            usize::from(compilation.span.instructions),
            layout.kinds.len(),
            "both backends must cover the identical slot list: {code:02x?}"
        );
        let access = plan.access_total;
        assert_eq!(access.byte_reads, compilation.byte_reads, "{code:02x?}");
        assert_eq!(access.word_reads, compilation.word_reads, "{code:02x?}");
        assert_eq!(access.dword_reads, compilation.dword_reads, "{code:02x?}");
        assert_eq!(access.byte_stores, compilation.byte_stores, "{code:02x?}");
        assert_eq!(access.word_stores, compilation.word_stores, "{code:02x?}");
        assert_eq!(access.dword_stores, compilation.dword_stores, "{code:02x?}");
    }
}

// ---------------------------------------------------------------------------------------
// Track C C1c increment 2: the Push/Pop stack-path battery (design section 5 test 7 and
// the M1 PUSH ESP / POP ESP pins).
// ---------------------------------------------------------------------------------------

/// Push/Pop matrix: register and immediate pushes (dword 0x68 and sign-extended 0x6a),
/// balanced pop sequences, an SS-relative Load between them (`mov ecx,[esp]` addresses
/// through SS as an ordinary DirectAddr segment), and the M1 pins: `PUSH ESP` stores the
/// PRE-decrement value (store-before-decrement) and `POP ESP` leaves ESP equal to the
/// LOADED value, not loaded plus 4 (read-increment-then-write, the load-bearing order).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_push_pop_matrix_and_esp_pins() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // Balanced register pushes and pops, crossing registers.
        assert_clif_forced_case(
            mode,
            &[0x50, 0x53, 0x59, 0x5a, 0xf4], // push eax; push ebx; pop ecx; pop edx
            forced_gpr(),
            0x202,
        );
        // Immediate pushes: dword and the sign-extended byte form.
        assert_clif_forced_case(
            mode,
            &[
                0x68, 0x44, 0x33, 0x22, 0x11, // push 0x11223344
                0x6a, 0x80, // push -0x80 (sign-extended)
                0x58, 0x5b, // pop eax; pop ebx
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
        // SS-relative Load: push eax; mov ecx,[esp] (SS-segment DirectAddr); pop edx.
        assert_clif_forced_case(
            mode,
            &[0x50, 0x8b, 0x0c, 0x24, 0x5a, 0xf4],
            forced_gpr(),
            0x202,
        );
        // PUSH ESP stores the PRE-decrement ESP: pop it into EAX and compare (the
        // interpreter is the oracle; equality pins the value byte-for-byte).
        assert_clif_forced_case(mode, &[0x54, 0x58, 0xf4], forced_gpr(), 0x202);
        // POP ESP receives the LOADED value: push esp; pop esp leaves ESP unchanged.
        assert_clif_forced_case(mode, &[0x54, 0x5c, 0xf4], forced_gpr(), 0x202);
        // POP ESP from an arbitrary pushed value (no further stack use afterwards).
        assert_clif_forced_case(mode, &[0x50, 0x5c, 0xf4], forced_gpr(), 0x202);
    }
}

/// Section 1.4's stack-width admission gate: under a 16-bit stack the growth walker stops
/// AT the Push/Pop slot exactly as an unclassifiable opcode would (no new stop tag), the
/// preceding register run still lowers, and the run stays byte-identical (the interpreter
/// retires the 16-bit-SP push canonically).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_sixteen_bit_stack_stops_push_pop_admission() {
    use crate::jit::clif::cache::walk_unit;

    let entry = 0x1000u32;
    // nop (starter); inc eax; push eax; pop ebx; hlt.
    let code: &[u8] = &[0x90, 0x40, 0x50, 0x5b, 0xf4];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    let ss16 = SegmentRegister {
        selector: 0x10,
        base: 0,
        limit: u32::MAX,
        access: 0x93,
        default_size_32: false,
    };

    let mut interp = generated_cpu(GswMode::Gsw586);
    let mut clif = generated_cpu(GswMode::Gsw586);
    clif.set_clif_backend_enabled(true);
    for cpu in [&mut interp, &mut clif] {
        cpu.registers.set_segment(SegmentIndex::Ss, ss16);
    }
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for _ in 0..4 {
        for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = [0x1122_3344, 0, 0, 0, 0x1_f000, 0, 0, 0];
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
        }
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(clif.timing_rem, interp.timing_rem);
    }
    // The walker stops growth AT the push: the walked unit holds only the inc.
    let layout = walk_unit(&clif, entry + 1, true).expect("walked layout");
    assert_eq!(
        layout.kinds.len(),
        1,
        "a 16-bit stack must stop growth at the Push slot"
    );
    // The 32-bit-stack shape of the same bytes admits all three slots, proving the stop
    // is the stack-width gate and not something else about the corpus.
    clif.registers
        .set_segment(SegmentIndex::Ss, SegmentRegister::flat(0x10, 0x93));
    let layout = walk_unit(&clif, entry + 1, true).expect("walked layout");
    assert_eq!(layout.kinds.len(), 3);
}

/// The SS-relative watched-store case: a PUSH whose target lands in the unit's own
/// watched chunk side-exits through the code-watch check BEFORE the store commits, with
/// ESP unmodified on the exit path (the SSA discipline: the side block's predecessors all
/// branch before the ESP redefinition), and the interpreter's canonical push produces the
/// identical final state. The pushed value equals the resident bytes, so G2 elides the
/// invalidation and the unit survives to exit again next pass.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_push_into_watched_page_side_exits_before_commit() {
    use crate::jit::clif::cache::{ClifUnitState, clif_key_for};

    let entry = 0x1000u32;
    // nop (starter); push eax; inc ebx; hlt. The unit spans 0x1001..0x1004 (chunk 0x100);
    // ESP starts at 0x1008, so the push writes 0x1004..0x1008: same page, same chunk.
    let code: &[u8] = &[0x90, 0x50, 0x43, 0xf4];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    // The bytes at 0x1004..0x1008 are the 0xf4 filler: pushing the same dword keeps the
    // store same-value, so G2 elides the invalidation and the unit survives.
    let same_value = u32::from_le_bytes([0xf4, 0xf4, 0xf4, 0xf4]);

    let mut interp = generated_cpu(GswMode::Gsw586);
    let mut clif = generated_cpu(GswMode::Gsw586);
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let pass = |interp: &mut CpuGsw,
                clif: &mut CpuGsw,
                interp_bus: &mut TestBus,
                clif_bus: &mut TestBus| {
        for (cpu, bus) in [
            (&mut *interp, &mut *interp_bus),
            (&mut *clif, &mut *clif_bus),
        ] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = [same_value, 0, 0, 0, 0x1008, 0, 0, 0];
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
        }
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(clif.timing_rem, interp.timing_rem);
    };
    for _ in 0..4 {
        pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    }
    let key = clif_key_for(&clif, entry + 1, true).expect("warm key");
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
    let before = clif.jit_clif_counters();
    assert!(before.entries > 0, "unit never entered: {before:#?}");
    pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    let after = clif.jit_clif_counters();
    assert!(
        after.mem_exit_code_watch > before.mem_exit_code_watch,
        "the watched-chunk push must exit through the code-watch check: {after:#?}"
    );
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
}

// ---------------------------------------------------------------------------------------
// Track C C1c increment 3: the read-only memory ALU battery (AluMemSource + TestImmMem).
// ---------------------------------------------------------------------------------------

/// AluMemSource across every op (0..=7) at dword width, the ADC/SBB entry-carry arms via
/// STC/CLC, CMP's no-write arm, and the word forms on the 586 persona. The memory operand
/// is C1b's register `b` replaced by a checked load; flags ride the identical lowering,
/// so the pending-descriptor bytes and raw eflags are compared byte-for-byte through the
/// forced harness.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_alu_mem_source_op_matrix() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for op in 0..=7u8 {
            // nop starter (the continuation probe lands on the memory op), then
            // op reg, [0x5000] over the 0xf4 filler bytes; dst varies with op.
            let opcode = (op << 3) | 3;
            let modrm = 0x05 | ((op & 7) << 3);
            assert_clif_forced_case(
                mode,
                &[0x90, opcode, modrm, 0x00, 0x50, 0x00, 0x00, 0xf4],
                forced_gpr(),
                0x202,
            );
        }
        // ADC/SBB with both entry-carry arms (the lazy and eager flag shapes); the
        // interpreted STC/CLC starter doubles as the continuation position.
        for (setcc, op) in [(0xf9u8, 2u8), (0xf8, 2), (0xf9, 3), (0xf8, 3)] {
            let opcode = (op << 3) | 3;
            assert_clif_forced_case(
                mode,
                &[setcc, opcode, 0x05, 0x00, 0x50, 0x00, 0x00, 0xf4],
                forced_gpr(),
                0x202,
            );
        }
        // An in-unit producer feeding the memory ADC: ADD sets carry, ADC consumes it.
        assert_clif_forced_case(
            mode,
            &[
                0x01, 0xd8, // add eax, ebx (sets CF from the forced gpr values)
                0x13, 0x0d, 0x00, 0x50, 0x00, 0x00, // adc ecx, [0x5000]
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
    }
    // Word forms, 586 only. The classifier's word gate admits only the CMP encodings
    // (0x39/0x3b) among the sub-0x40 ALU opcodes, so CMP is the word AluMemSource shape.
    assert_clif_forced_case(
        GswMode::Gsw586,
        &[
            0x90, // nop starter
            0x66, 0x3b, 0x1d, 0x04, 0x50, 0x00, 0x00, // cmp bx, [0x5004]
            0x66, 0x3b, 0x05, 0x00, 0x50, 0x00, 0x00, // cmp ax, [0x5000]
            0xf4,
        ],
        forced_gpr(),
        0x202,
    );
}

/// The M4 byte-width AluMemSource asymmetry, both halves: (a) the byte ALU-mem-source
/// encodings (form 2, e.g. 0x02 `add r8, r/m8`) are not classified at all, so NEITHER
/// backend admits them and growth stops identically (design section 1.1's
/// excluded-by-construction discipline, proven rather than asserted); (b) a hand-built
/// byte AluMemSource kind contributes ZERO byte reads through the shared accessor
/// (`byte_reads` deliberately omits it), so `plan_unit` matches Direct exactly on the
/// shape by shared code.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_byte_alu_mem_source_asymmetry() {
    use crate::jit::clif::cache::walk_unit;
    use crate::jit::clif::lower::plan_unit;
    use crate::jit::direct::{DirectAddr, DirectKind, MemoryWidth};

    // (a) Non-admission: nop; inc eax; add al,[0x5000]; hlt. The walker stops BEFORE the
    // 0x02 form on both backends; the run stays byte-identical (the forced harness
    // requires at least one clif entry, satisfied by the inc-only unit).
    let clif = run_clif_forced_case(
        GswMode::Gsw586,
        &[0x90, 0x40, 0x02, 0x05, 0x00, 0x50, 0x00, 0x00, 0xf4],
        forced_gpr(),
        0x202,
    );
    let layout = walk_unit(&clif, 0x1001, true).expect("walked layout");
    assert_eq!(
        layout.kinds.len(),
        1,
        "the byte ALU-mem-source form must stop growth (not classified)"
    );

    // (b) The accessor asymmetry, pinned at the plan level: byte width contributes zero
    // reads (byte_reads omits AluMemSource), while word/dword contribute one, all through
    // DirectKind's own accessors.
    let addr = DirectAddr {
        segment: SegmentIndex::Ds,
        base: None,
        index: None,
        scale: 1,
        disp: 0x5000,
    };
    for (width, expect_byte, expect_word, expect_dword) in [
        (MemoryWidth::Byte, 0u8, 0u8, 0u8),
        (MemoryWidth::Word, 0, 1, 0),
        (MemoryWidth::Dword, 0, 0, 1),
    ] {
        let kinds = [DirectKind::AluMemSource {
            op: 0,
            dst: 0,
            width,
            addr,
        }];
        let plan = plan_unit(&kinds, true);
        assert_eq!(plan.access_total.byte_reads, expect_byte);
        assert_eq!(plan.access_total.word_reads, expect_word);
        assert_eq!(plan.access_total.dword_reads, expect_dword);
        assert_eq!(plan.access_total.stores(), 0, "read-only form");
    }
}

// ---------------------------------------------------------------------------------------
// Track C C1c increment 4: the AluMemDest read-modify-write battery.
// ---------------------------------------------------------------------------------------

/// AluMemDest across every op and immediate form: the register-source encodings
/// (`(op << 3) | 1`) for ops 0..=6 plus CMP (0x39), the byte 0x80 group with imm8 (all
/// eight ops, two-lane slots: displacement AND operand immediate), the dword 0x81 imm32
/// and sign-extended 0x83 imm8 groups, the ADC/SBB entry-carry arms, and the word CMP
/// form (the only word AluMemDest the classifier's word gate admits). CMP's read-only
/// early-return leaves memory untouched with the lazy Sub descriptor; every write form
/// commits result and flags only past the full check list.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_alu_mem_dest_op_matrix() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // Register-source dword forms: op [0x5000], reg (src varies with op).
        for op in 0..=7u8 {
            let opcode = (op << 3) | 1;
            let modrm = 0x05 | ((op & 7) << 3);
            assert_clif_forced_case(
                mode,
                &[0x90, opcode, modrm, 0x00, 0x50, 0x00, 0x00, 0xf4],
                forced_gpr(),
                0x202,
            );
        }
        // Byte 0x80 group, all eight ops, imm8 operands over the 0xf4 filler.
        for op in 0..=7u8 {
            assert_clif_forced_case(
                mode,
                &[
                    0x90,
                    0x80,
                    0x05 | (op << 3),
                    0x00,
                    0x50,
                    0x00,
                    0x00,
                    0x5a,
                    0xf4,
                ],
                forced_gpr(),
                0x202,
            );
        }
        // Dword immediate groups: 0x81 imm32 and the sign-extended 0x83 imm8.
        for op in [0u8, 1, 4, 5, 6, 7] {
            assert_clif_forced_case(
                mode,
                &[
                    0x90,
                    0x81,
                    0x05 | (op << 3),
                    0x00,
                    0x50,
                    0x00,
                    0x00,
                    0x44,
                    0x33,
                    0x22,
                    0x11,
                    0xf4,
                ],
                forced_gpr(),
                0x202,
            );
            assert_clif_forced_case(
                mode,
                &[
                    0x90,
                    0x83,
                    0x05 | (op << 3),
                    0x00,
                    0x50,
                    0x00,
                    0x00,
                    0x80,
                    0xf4,
                ],
                forced_gpr(),
                0x202,
            );
        }
        // ADC/SBB memory destinations, both entry-carry arms, via 0x81 /2 and /3.
        for (setcc, op) in [(0xf9u8, 2u8), (0xf8, 2), (0xf9, 3), (0xf8, 3)] {
            assert_clif_forced_case(
                mode,
                &[
                    setcc,
                    0x81,
                    0x05 | (op << 3),
                    0x00,
                    0x50,
                    0x00,
                    0x00,
                    0xff,
                    0xff,
                    0xff,
                    0x7f,
                    0xf4,
                ],
                forced_gpr(),
                0x202,
            );
        }
        // Register-source ADC/SBB memory destinations (0x11/0x19).
        for (setcc, opcode) in [(0xf9u8, 0x11u8), (0xf8, 0x11), (0xf9, 0x19), (0xf8, 0x19)] {
            assert_clif_forced_case(
                mode,
                &[setcc, opcode, 0x1d, 0x00, 0x50, 0x00, 0x00, 0xf4],
                forced_gpr(),
                0x202,
            );
        }
    }
    // Word CMP memory-destination form, 586 only (0x39 is in the classifier's word list).
    assert_clif_forced_case(
        GswMode::Gsw586,
        &[
            0x90, // nop starter
            0x66, 0x39, 0x05, 0x00, 0x50, 0x00, 0x00, // cmp word [0x5000], ax
            0xf4,
        ],
        forced_gpr(),
        0x202,
    );
}

/// The provisional-effect fault case (design section 5 test 8): the WRITE-side permission
/// check fails while the READ would have succeeded (a ring-3 RMW into a user READ-ONLY
/// page). The unit side-exits BEFORE any read or candidate computation commits anything:
/// destination memory, registers, the pending-descriptor bytes, and EFLAGS all stay
/// byte-identical to the interpreter-only run, whose canonical re-execution raises the
/// same fault (checked as outcome equality, not just no-crash).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_alu_mem_dest_write_denied_keeps_all_state() {
    let entry = 0x1000u32;
    let ring3_cpu = || {
        let mut cpu = generated_cpu(GswMode::Gsw586);
        cpu.control.cr0 |= CR0_PG | CR0_WP;
        cpu.control.cr3 = 0x3000;
        cpu.registers.set_segment(
            SegmentIndex::Cs,
            SegmentRegister {
                selector: 0x0b,
                base: 0,
                limit: u32::MAX,
                access: 0xfb,
                default_size_32: true,
            },
        );
        for segment in [
            SegmentIndex::Ds,
            SegmentIndex::Ss,
            SegmentIndex::Es,
            SegmentIndex::Fs,
            SegmentIndex::Gs,
        ] {
            cpu.registers.set_segment(
                segment,
                SegmentRegister {
                    selector: 0x13,
                    base: 0,
                    limit: u32::MAX,
                    access: 0xf3,
                    default_size_32: true,
                },
            );
        }
        cpu.cpl = 3;
        cpu
    };
    // Page 0x11 is user read-only (flags 5); everything else user RW (flags 7). The
    // leading load of the RO page is PERMITTED and populates its FastMap flags, so the
    // RMW's write-permission check is the one that fires (not the kind/unavailable one).
    let code: &[u8] = &[
        0x40, // inc eax
        0xa1, 0x00, 0x10, 0x01, 0x00, // mov eax, [0x11000] (permitted read, populates)
        0x01, 0x1d, 0x00, 0x10, 0x01, 0x00, // add [0x11000], ebx (write-denied RMW)
        0xf4,
    ];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);
    memory[0x3000..0x3004].copy_from_slice(&0x4007u32.to_le_bytes());
    for page in 0..32u32 {
        let flags = if page == 0x11 { 5 } else { 7 };
        let pte = (page << 12) | flags;
        let offset = 0x4000 + page as usize * 4;
        memory[offset..offset + 4].copy_from_slice(&pte.to_le_bytes());
    }

    let mut interp = ring3_cpu();
    let mut clif = ring3_cpu();
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    for _ in 0..4 {
        let mut results = Vec::new();
        for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = [0x1122_3344, 0, 0, 0x5566_7788, 0x1_f000, 0, 0, 0];
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            let mut outcome = Ok(());
            for _ in 0..64 {
                match cpu.run_budgeted(bus, 4096) {
                    Ok(o) if o.halted => break,
                    Ok(_) => {}
                    Err(error) => {
                        outcome = Err(error);
                        break;
                    }
                }
            }
            results.push(outcome);
        }
        assert_eq!(results[0], results[1], "outcome differs");
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.pending_flags, interp.pending_flags);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
    }
    let counters = clif.jit_clif_counters();
    assert!(counters.entries > 0, "{counters:#?}");
    assert!(
        counters.mem_exit_permission > 0,
        "the denied RMW must exit through the write-permission check: {counters:#?}"
    );
}

/// A watched-destination RMW: `or dword [own unit bytes], 0` writes the identical value,
/// so the inline code-watch check side-exits before ANY commit (no store, no flag
/// change), the interpreter's canonical RMW elides the invalidation (G2), the unit
/// survives, and state plus timing stay byte-identical across passes.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_alu_mem_dest_watched_dest_exits_before_commit() {
    use crate::jit::clif::cache::{ClifUnitState, clif_key_for};

    let entry = 0x1000u32;
    // nop; or dword [0x1004], 0 (0x1004 holds the instruction's own displacement/imm
    // bytes; OR with zero leaves them unchanged); inc ebx; hlt. Unit spans 0x1001..0x100d.
    let code: &[u8] = &[
        0x90, 0x81, 0x0d, 0x04, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x43, 0xf4,
    ];
    let mut memory = vec![0xf4u8; MEMORY_LEN];
    memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);

    let mut interp = generated_cpu(GswMode::Gsw586);
    let mut clif = generated_cpu(GswMode::Gsw586);
    clif.set_clif_backend_enabled(true);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    let mut clif_bus = TestBus::with_memory(memory.clone());
    for bus in [&mut interp_bus, &mut clif_bus] {
        bus.direct_pages_enabled = true;
        bus.direct_page_clocks = true;
    }
    let pass = |interp: &mut CpuGsw,
                clif: &mut CpuGsw,
                interp_bus: &mut TestBus,
                clif_bus: &mut TestBus| {
        for (cpu, bus) in [
            (&mut *interp, &mut *interp_bus),
            (&mut *clif, &mut *clif_bus),
        ] {
            bus.memory.copy_from_slice(&memory);
            bus.trace.clear();
            cpu.halted = false;
            cpu.registers.gpr = forced_gpr();
            cpu.registers.eflags = 0x202;
            cpu.pending_flags = PendingFlags::default();
            cpu.set_eip(entry);
            cpu.elapsed_clocks = 0;
            cpu.core_clocks_so_far = 0;
            cpu.timing_rem = 0;
            while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
        }
        assert_eq!(clif.registers, interp.registers);
        assert_eq!(clif.pending_flags, interp.pending_flags);
        assert_eq!(clif.eflags(), interp.eflags());
        assert_eq!(clif_bus.memory, interp_bus.memory);
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks()
        );
        assert_eq!(clif.timing_rem, interp.timing_rem);
    };
    for _ in 0..4 {
        pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    }
    let key = clif_key_for(&clif, entry + 1, true).expect("warm key");
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
    let before = clif.jit_clif_counters();
    pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
    let after = clif.jit_clif_counters();
    assert!(
        after.mem_exit_code_watch > before.mem_exit_code_watch,
        "the watched-destination RMW must exit through the code-watch check: {after:#?}"
    );
    assert!(matches!(
        clif.jit_direct.clif_units.state(key),
        Some(ClifUnitState::Compiled(_))
    ));
}

// ---------------------------------------------------------------------------------------
// Track C C1c increment 5: the RmwIncDec + DoubleShiftMem battery.
// ---------------------------------------------------------------------------------------

/// Memory INC/DEC across word and dword widths with the CF-preservation pins: the
/// pending-descriptor bytes (cf_override tag bits 16/17, b == 1) are compared
/// byte-for-byte with entry CF from STC/CLC and from an in-unit pending producer.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_rmw_inc_dec_matrix_and_cf_pins() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        // Dword INC/DEC over the 0xf4 filler, entry CF both ways.
        for modrm in [0x05u8, 0x0d] {
            for setcc in [0xf9u8, 0xf8] {
                assert_clif_forced_case(
                    mode,
                    &[setcc, 0xff, modrm, 0x00, 0x50, 0x00, 0x00, 0xf4],
                    forced_gpr(),
                    0x202,
                );
            }
        }
        // In-unit pending producer: ADD sets CF, the memory INC/DEC must preserve it
        // through cf_override while its own descriptor takes over.
        assert_clif_forced_case(
            mode,
            &[
                0x01, 0xd8, // add eax, ebx (sets CF from the forced gpr values)
                0xff, 0x05, 0x00, 0x50, 0x00, 0x00, // inc dword [0x5000]
                0xff, 0x0d, 0x04, 0x50, 0x00, 0x00, // dec dword [0x5004]
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
        // Wrap edges: 0xffffffff increments to zero, zero decrements to 0xffffffff.
        assert_clif_forced_case(
            mode,
            &[
                0x90, // nop starter
                0xc7, 0x05, 0x00, 0x50, 0x00, 0x00, 0xff, 0xff, 0xff,
                0xff, // mov dword [0x5000], -1
                0xff, 0x05, 0x00, 0x50, 0x00, 0x00, // inc dword [0x5000] (wraps to 0, ZF)
                0xc7, 0x05, 0x04, 0x50, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, // mov dword [0x5004], 0
                0xff, 0x0d, 0x04, 0x50, 0x00, 0x00, // dec dword [0x5004] (wraps to -1)
                0xf4,
            ],
            forced_gpr(),
            0x202,
        );
    }
    // Word forms, 586 only (0xff is in the classifier's word list).
    assert_clif_forced_case(
        GswMode::Gsw586,
        &[
            0xf9, // stc
            0x66, 0xff, 0x05, 0x00, 0x50, 0x00, 0x00, // inc word [0x5000]
            0x66, 0xff, 0x0d, 0x02, 0x50, 0x00, 0x00, // dec word [0x5002]
            0xf4,
        ],
        forced_gpr(),
        0x202,
    );
}

/// The m2 byte-form non-existence pin, mirroring the byte-AluMemSource shape: the 0xFE
/// group (byte INC/DEC r/m) has no classify arm at all, so NEITHER backend admits it and
/// growth stops identically; and a hand-built byte RmwIncDec contributes nothing through
/// any accessor lane (it is absent from byte_reads and byte_stores by design).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_byte_rmw_inc_dec_non_existence() {
    use crate::jit::clif::cache::walk_unit;
    use crate::jit::clif::lower::plan_unit;
    use crate::jit::direct::{DirectAddr, DirectKind, MemoryWidth};

    // nop; inc eax; inc byte [0x5000] (0xFE /0, unclassified); hlt.
    let clif = run_clif_forced_case(
        GswMode::Gsw586,
        &[0x90, 0x40, 0xfe, 0x05, 0x00, 0x50, 0x00, 0x00, 0xf4],
        forced_gpr(),
        0x202,
    );
    let layout = walk_unit(&clif, 0x1001, true).expect("walked layout");
    assert_eq!(
        layout.kinds.len(),
        1,
        "the byte INC/DEC group must stop growth (not classified)"
    );

    let addr = DirectAddr {
        segment: SegmentIndex::Ds,
        base: None,
        index: None,
        scale: 1,
        disp: 0x5000,
    };
    let kinds = [DirectKind::RmwIncDec {
        is_dec: false,
        width: MemoryWidth::Byte,
        addr,
    }];
    let plan = plan_unit(&kinds, true);
    assert_eq!(plan.access_total.reads(), 0, "no byte RmwIncDec read lane");
    assert_eq!(
        plan.access_total.stores(),
        0,
        "no byte RmwIncDec store lane"
    );
}

/// SHLD/SHRD memory destinations across the count classes (0, 1, > 1, 31, and a
/// masked-to-zero 32), immediate and CL counts, both directions, with a live in-unit
/// pending descriptor feeding the commit's fallback arms.
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_forced_double_shift_mem_count_classes() {
    for mode in [GswMode::Gsw486, GswMode::Gsw586] {
        for opcode in [0xa4u8, 0xac] {
            // Immediate counts: the zero-count no-op, the count==1 defined-OF arm, a
            // middle count, 31, and 32 (masked to zero).
            for count in [0u8, 1, 7, 31, 32] {
                assert_clif_forced_case(
                    mode,
                    &[
                        0x90, 0x0f, opcode, 0x1d, 0x00, 0x50, 0x00, 0x00, count, 0xf4,
                    ],
                    forced_gpr(),
                    0x202,
                );
            }
        }
        // CL counts (forced_gpr's ECX low byte is 0x11 = 17), both directions, plus a
        // pending producer so the commit's live-descriptor fallback arm runs.
        for opcode in [0xa5u8, 0xad] {
            assert_clif_forced_case(
                mode,
                &[
                    0x01, 0xd8, // add eax, ebx (a live pending descriptor)
                    0x0f, opcode, 0x35, 0x00, 0x50, 0x00, 0x00, // shld/shrd [0x5000], esi, cl
                    0xf4,
                ],
                forced_gpr(),
                0x202,
            );
        }
    }
}

/// The two RMW check orderings, pinned through the diagnostic counters over a WATCHED
/// destination: RmwIncDec checks code-watch BEFORE any read (emit.rs:1873's front-loaded
/// check), AluMemDest and DoubleShiftMem after the candidate; all three side-exit with
/// the CodeWatch reason and zero guest-visible effects, and the failing slot charges
/// nothing in every case (the cum-prefix arrays exclude it identically under both
/// orderings, so the orderings are charge-equivalent by construction; the design's
/// fidelity note makes the emitted ORDER the mirrored property, not an observable one).
#[test]
#[cfg(all(
    feature = "clif-backend",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn clif_rmw_watched_dest_orderings_exit_with_code_watch() {
    use crate::jit::clif::cache::{ClifUnitState, clif_key_for};

    // Three shapes hitting the unit's own watched chunk. OR with 0 and SHLD by 0 target
    // the unit's own bytes value-preservingly (G2 elides the invalidation); the INC
    // targets the PADDING dword just past its unit's span but inside the unit's watched
    // 16-byte chunk, touching no decoded byte, so it is a chunk-granular conservative
    // watch hit: the value CHANGES each pass yet the interpreter's canonical INC hits no
    // code and invalidates nothing. All three units survive across passes and every pass
    // exits through the watch check.
    let cases: [(&[u8], bool); 3] = [
        // nop; inc dword [0x1008]; hlt: the front-loaded pre-read watch check (the
        // RmwIncDec-only ordering) over the same-chunk padding target.
        (&[0x90, 0xff, 0x05, 0x08, 0x10, 0x00, 0x00, 0xf4], true),
        // nop; or dword [0x1004], 0; inc ebx; hlt: value-preserving AluMemDest.
        (
            &[
                0x90, 0x81, 0x0d, 0x04, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x43, 0xf4,
            ],
            true,
        ),
        // nop; shld [0x1004], ebx, 0; inc ebx; hlt: zero count stores the old value.
        (
            &[
                0x90, 0x0f, 0xa4, 0x1d, 0x04, 0x10, 0x00, 0x00, 0x00, 0x43, 0xf4,
            ],
            true,
        ),
    ];
    for (code, value_preserving) in cases {
        let entry = 0x1000u32;
        let mut memory = vec![0xf4u8; MEMORY_LEN];
        memory[entry as usize..entry as usize + code.len()].copy_from_slice(code);

        let mut interp = generated_cpu(GswMode::Gsw586);
        let mut clif = generated_cpu(GswMode::Gsw586);
        clif.set_clif_backend_enabled(true);
        let mut interp_bus = TestBus::with_memory(memory.clone());
        let mut clif_bus = TestBus::with_memory(memory.clone());
        for bus in [&mut interp_bus, &mut clif_bus] {
            bus.direct_pages_enabled = true;
            bus.direct_page_clocks = true;
        }
        let pass = |interp: &mut CpuGsw,
                    clif: &mut CpuGsw,
                    interp_bus: &mut TestBus,
                    clif_bus: &mut TestBus| {
            for (cpu, bus) in [
                (&mut *interp, &mut *interp_bus),
                (&mut *clif, &mut *clif_bus),
            ] {
                bus.memory.copy_from_slice(&memory);
                bus.trace.clear();
                cpu.halted = false;
                cpu.registers.gpr = forced_gpr();
                cpu.registers.eflags = 0x202;
                cpu.pending_flags = PendingFlags::default();
                cpu.set_eip(entry);
                cpu.elapsed_clocks = 0;
                cpu.core_clocks_so_far = 0;
                cpu.timing_rem = 0;
                while !cpu.run_budgeted(bus, 4096).expect("runs").halted {}
            }
            assert_eq!(clif.registers, interp.registers, "{code:02x?}");
            assert_eq!(clif.pending_flags, interp.pending_flags, "{code:02x?}");
            assert_eq!(clif.eflags(), interp.eflags(), "{code:02x?}");
            assert_eq!(clif_bus.memory, interp_bus.memory, "{code:02x?}");
            assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "{code:02x?}");
            assert_eq!(
                clif_bus.trace.elapsed_clocks(),
                interp_bus.trace.elapsed_clocks(),
                "{code:02x?}"
            );
        };
        if value_preserving {
            for _ in 0..4 {
                pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
            }
            let key = clif_key_for(&clif, entry + 1, true).expect("warm key");
            assert!(matches!(
                clif.jit_direct.clif_units.state(key),
                Some(ClifUnitState::Compiled(_))
            ));
            let before = clif.jit_clif_counters();
            pass(&mut interp, &mut clif, &mut interp_bus, &mut clif_bus);
            let after = clif.jit_clif_counters();
            assert!(
                after.mem_exit_code_watch > before.mem_exit_code_watch,
                "the watched-destination RMW must exit through the code-watch check: \
                 {code:02x?}, {after:#?}"
            );
            assert_eq!(
                after.mem_exit_unavailable_or_kind, before.mem_exit_unavailable_or_kind,
                "the watch reason, not a bias miss: {code:02x?}"
            );
            assert!(matches!(
                clif.jit_direct.clif_units.state(key),
                Some(ClifUnitState::Compiled(_))
            ));
        } else {
            unreachable!("every current case survives across passes");
        }
    }
}
