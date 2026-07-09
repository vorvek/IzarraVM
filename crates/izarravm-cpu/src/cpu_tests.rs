use super::*;
use izarravm_bus::{BusCycle, BusTrace, BusWidth};
use izarravm_bus::{DirectMemoryRead, DirectMemoryWrite};

/// The JIT's emitted native code addresses `gpr[i]` as `[regs_ptr + 4*i]`, relying on
/// `Registers` being `repr(C)` with `gpr` as the first field. A rustc or field reorder that broke
/// this offset would silently corrupt guest state through wrong native loads/stores, so this
/// test freezes the layout assumption the JIT bakes into its emitted bytes. The eip offset is
/// asserted too (the dispatch reads it); eflags follows eip at +4.
#[test]
fn registers_repr_c_offsets_are_stable() {
    // gpr is the first field of repr(C) Registers: offset 0, 4-byte element stride.
    assert_eq!(core::mem::offset_of!(Registers, gpr), 0);
    assert_eq!(core::mem::size_of::<u32>(), 4);
    // eip sits after gpr (32 bytes) + segments ([SegmentRegister; 6]). repr(C) guarantees this
    // declaration order is the memory order; eflags immediately follows eip at +4.
    let eip_off = core::mem::offset_of!(Registers, eip);
    assert_eq!(eip_off, 32 + core::mem::size_of::<[SegmentRegister; 6]>());
    assert_eq!(core::mem::offset_of!(Registers, eflags), eip_off + 4);
}

/// The v2 region emitter bakes `offset_of!(Cpu386, registers)` into its emitted bytes (the
/// prologue computes `regs_ptr = cpu_ptr + regs_offset`, and inline slots address gpr as
/// `[regs_ptr + 4*i]`). Cpu386 is NOT repr(C), so rustc is free to reorder its fields; this
/// test pins the current offset so a rustc version bump that moved `registers` is caught here
/// (the emitter reads the offset at emit time, so a changed value still produces correct code,
/// but the assertion documents the layout and guards against a silent perf shift from a
/// changed cache-line placement of gpr).
#[test]
#[cfg(feature = "jit")]
fn cpu_registers_field_offset_is_stable() {
    let off = core::mem::offset_of!(Cpu386, registers);
    // The current layout places `registers` at a non-zero offset (rustc reorders Cpu386's
    // fields for alignment). The emitter handles any value (it bakes `offset_of!` at emit
    // time, verified by the differential suites jit_region + jit_general); this assertion
    // freezes the known position so a change is visible. The constant tracks the live layout
    // (456 -> 464 when Round 1 added the `jit_table_clears` u64 to PerfCounters, which precedes
    // `registers`; the emitter re-reads the offset, so this is a documentation update).
    assert_eq!(
        off, 464,
        "Cpu386.registers offset moved; update the emitter's baked offset"
    );
}

#[test]
#[cfg(feature = "jit")]
fn region_ctx_fn_pointer_offsets() {
    // Pin ALL offsets the emitted native code reads/writes so a field reorder is caught.
    use jit::step::RegionCtx;
    assert_eq!(core::mem::offset_of!(RegionCtx, step_fn), 0);
    assert_eq!(core::mem::offset_of!(RegionCtx, inline_step_fn), 8);
    assert_eq!(core::mem::offset_of!(RegionCtx, set_pending_add_fn), 16);
    assert_eq!(core::mem::offset_of!(RegionCtx, set_shift_flags_fn), 24);
    assert_eq!(core::mem::offset_of!(RegionCtx, charge_fetch_fn), 32);
    assert_eq!(core::mem::offset_of!(RegionCtx, bus_clocks_fn), 40);
    assert_eq!(core::mem::offset_of!(RegionCtx, line_live_fn), 48);
    // Pending flags offset for direct write in v2 inlining (slice 2+).
    assert_eq!(core::mem::offset_of!(Cpu386, pending_flags), 3912);
    // Verify the timing-field offsets the native cap check uses.
    let raw_off = core::mem::offset_of!(RegionCtx, raw_clocks);
    eprintln!("raw_clocks offset = {raw_off}");
    assert_eq!(raw_off, 88);
    let rt_off = core::mem::offset_of!(RegionCtx, run_total_at_entry);
    eprintln!("run_total_at_entry offset = {rt_off}");
    let cap_off = core::mem::offset_of!(RegionCtx, cap);
    eprintln!("cap offset = {cap_off}");
    // `d` is read by LIVE emitted code now (the native fold's line_live arg loads it via D_OFF=144),
    // so pin it — a reorder that moved it while leaving the fold fields > 127 would slip past the
    // other asserts and feed jit_line_live the wrong decode-line D bit.
    let d_off = core::mem::offset_of!(RegionCtx, d);
    eprintln!("d offset = {d_off}");
    assert_eq!(
        d_off, 144,
        "RegionCtx.d moved; update D_OFF in jit/block.rs"
    );
    // The native cost-fold reads these two by disp32 (both are past 127). The emit bakes
    // offset_of! at emit time, so a reorder still produces correct code; assert they stay in the
    // disp32 range so a future field placement that pulled them under 128 (silently switching the
    // emit's addressing assumption) is caught.
    let folded_off = core::mem::offset_of!(RegionCtx, folded_raw_bus);
    let cost_off = core::mem::offset_of!(RegionCtx, fold_bus_cost);
    let fetch_off = core::mem::offset_of!(RegionCtx, fetch_cost);
    // `store_finish_fn` is a fn-pointer the native STORE fold loads by disp32 and calls; keep it in
    // the disp32 range alongside the other fold fields.
    let finish_off = core::mem::offset_of!(RegionCtx, store_finish_fn);
    eprintln!(
        "folded_raw_bus offset = {folded_off}, fold_bus_cost offset = {cost_off}, fetch_cost offset = {fetch_off}, store_finish_fn offset = {finish_off}"
    );
    assert!(
        folded_off > 127 && cost_off > 127 && fetch_off > 127 && finish_off > 127,
        "fold fields must be disp32"
    );
}

/// The JIT's `jit_set_pending_add` helper must construct the identical pending descriptor the
/// interpreter's `alu_add(a, b, 0, Dword)` does, so that a later flag read (or materialization)
/// sees the same six arithmetic bits. Swept across operand pairs that exercise the carry,
/// zero, sign, overflow, half-carry, and parity paths. The comparison goes through
/// `materialized_eflags`, the same reader the interpreter uses, so it is exact.
#[cfg(feature = "jit")]
#[test]
fn jit_set_pending_add_matches_alu_add() {
    let probes = [
        (0u32, 0u32),
        (1, 1),
        (0xffff_ffff, 1),
        (0x7fff_ffff, 1),
        (0x8000_0000, 0x8000_0000),
        (0x0f, 0x01),
        (0x1f, 0x01),
        (0x1234_5678, 0x9abc_def0),
        (0xffff_ffff, 0xffff_ffff),
        (0x0000_00ff, 0x0000_0001),
    ];
    for &(a, b) in &probes {
        let mut ref_cpu = Cpu386::default();
        ref_cpu.alu_add(a, b, 0, BusWidth::Dword);
        let ref_ef = ref_cpu.materialized_eflags();

        let mut jit_cpu = Cpu386::default();
        jit_cpu.jit_set_pending_add(a, b);
        let jit_ef = jit_cpu.materialized_eflags();

        assert_eq!(
            jit_ef, ref_ef,
            "jit_set_pending_add({a:#x}, {b:#x}) flags diverge from alu_add"
        );
        // The descriptor itself must match too (cf_override, op, width, result).
        assert_eq!(
            jit_cpu.pending_flags, ref_cpu.pending_flags,
            "jit_set_pending_add({a:#x}, {b:#x}) descriptor diverges"
        );
    }
}

/// The JIT's `jit_set_shift_flags_shr` helper must leave the identical flag state the
/// interpreter's `shift_rotate(5, value, count, Dword)` does, for every count 0..=31 and a set
/// of values that exercise CF (last bit out), OF (count==1 MSB), ZF/SF/PF (result), and the
/// AF/OF-preserved paths (count != 1). This is the hardest correctness property of the inline
/// SHR slots: a divergence here would corrupt the jnz back-edge decision.
#[cfg(feature = "jit")]
#[test]
fn jit_set_shift_flags_shr_matches_shift_rotate() {
    let values = [
        0u32,
        1,
        0x8000_0000,
        0xffff_ffff,
        0x7fff_ffff,
        0x4000_0000,
        0x0200_0000, // bit 25 set: CF probe for count 26
        0x0100_0000, // bit 24 set: CF probe for count 25 (the drawcolumn shift)
        0x1234_5678,
        0x0000_0001,
    ];
    for &value in &values {
        for count in 0u8..=31 {
            let mut ref_cpu = Cpu386::default();
            // Seed a non-trivial pending descriptor first, so the slow path of
            // set_shift_result_flags (fold-then-eager) is exercised, matching the real loop
            // where an earlier add slot leaves a descriptor outstanding.
            ref_cpu.alu_add(0x1000, 0x2000, 0, BusWidth::Dword);
            ref_cpu.shift_rotate(5, value, count, BusWidth::Dword);
            let ref_ef = ref_cpu.materialized_eflags();

            let mut jit_cpu = Cpu386::default();
            jit_cpu.alu_add(0x1000, 0x2000, 0, BusWidth::Dword);
            jit_cpu.jit_set_shift_flags_shr(value, count);
            let jit_ef = jit_cpu.materialized_eflags();

            assert_eq!(
                jit_ef, ref_ef,
                "jit_set_shift_flags_shr({value:#x}, {count}) flags diverge from shift_rotate"
            );
            // No descriptor should be outstanding after a shift (shifts materialize eagerly).
            assert_eq!(
                jit_cpu.pending_flags.tag & (1u32 << 31) != 0,
                ref_cpu.pending_flags.tag & (1u32 << 31) != 0,
                "jit_set_shift_flags_shr({value:#x}, {count}) pending-state diverges"
            );
        }
    }
}

/// G0' CPU-ceiling probe (2026-07-07 JIT/perf plan): how much faster is fully register-allocated
/// native codegen of a hot loop than our interpreter? Runs a 32-bit flat drawcolumn-shaped loop
/// (15 instructions, 7 memory ops) through the REAL interpreter (`run_straight_line`) and through
/// a hand-emitted native version that keeps every guest register in a host register and folds the
/// texture base into a host pointer so each guest base+index memory operand lowers to one host SIB
/// access (no per-access address-add, which would clobber the loop's live flags). The two runs
/// execute on identical fresh memory and their framebuffers are compared byte-for-byte, so a
/// codegen bug fails the test instead of faking a speed number. The speedup is an OPTIMISTIC
/// ceiling (best-case register allocation + raw-pointer memory vs a lean TestBus interpreter); the
/// realistic dynarec lands below it. Run:
///   cargo test -j8 -p izarravm-cpu --release --features jit g0_prime_cpu_ceiling -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_cpu_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const CODE: u32 = 0x0000;
    const TEX: u32 = 0x1000; // 512-byte texture region
    const COUNT_ADDR: u32 = 0x3000;
    const FB: u32 = 0x0010_0000; // framebuffer
    const STRIDE: u32 = 0x50; // guest edi advance per iteration (bytes)
    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 200_000;
    const TRIALS: usize = 7;
    const FB_LEN: usize = ITERS as usize * STRIDE as usize;
    const MEM_LEN: usize = FB as usize + FB_LEN + 0x1000;

    // --- guest loop bytes (32-bit); a trailing HLT ends run_straight_line ---
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP1
    code.extend_from_slice(&STEP1.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x07]); // mov [edi],eax
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]); // add ebp,STEP2
    code.extend_from_slice(&STEP2.to_le_bytes());
    code.extend_from_slice(&[0x89, 0x5F, 0x04]); // mov [edi+4],ebx
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x81, 0xC7]); // add edi,STRIDE
    code.extend_from_slice(&STRIDE.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    code.extend_from_slice(&[0xFF, 0x0D]); // dec dword [COUNT_ADDR]
    code.extend_from_slice(&COUNT_ADDR.to_le_bytes());
    code.extend_from_slice(&[0x8B, 0x04, 0x0E]); // mov eax,[esi+ecx]
    code.extend_from_slice(&[0x8B, 0x1C, 0x16]); // mov ebx,[esi+edx]
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8; // back to CODE (offset 0)
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt
    assert!(
        code.len() < TEX as usize,
        "code overruns the texture region"
    );

    let build_mem = || {
        let mut m = vec![0u8; MEM_LEN];
        m[..code.len()].copy_from_slice(&code);
        for i in 0..512u32 {
            m[(TEX + i) as usize] = (i as u8).wrapping_mul(37).wrapping_add(11);
        }
        m[COUNT_ADDR as usize..COUNT_ADDR as usize + 4].copy_from_slice(&ITERS.to_le_bytes());
        m
    };

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };
    let setup = |cpu: &mut Cpu386| {
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b)); // 32-bit code
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93)); // data
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Es, seg(0x10, 0x93));
        cpu.registers.eip = CODE;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(TEX);
        cpu.registers.set_edi(FB);
    };

    // --- native emission: guest regs pinned in host regs, memory via host pointers ---
    // ebp=R8 ecx=R9 edx=R10 eax=R11 ebx=RBX ; esi_host=R12 (ram+TEX) edi_host=R13 (ram+FB) count=R14
    // arg0(RCX)=ram_base, arg1(RDX)=iters.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.mov_r64_r64(Reg::R12, Reg::RCX);
        e.add_r64_imm32(Reg::R12, TEX); // esi_host = ram_base + TEX
        e.mov_r64_r64(Reg::R13, Reg::RCX);
        e.add_r64_imm32(Reg::R13, FB); // edi_host = ram_base + FB
        e.mov_r32_r32(Reg::R14, Reg::RDX); // count = iters
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1); // add ebp,STEP1
        e.store_r32_disp8(Reg::R13, 0, Reg::R11); // mov [edi],eax
        e.shr_r32_imm8(Reg::R9, 24); // shr ecx,24
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2); // add ebp,STEP2
        e.store_r32_disp8(Reg::R13, 4, Reg::RBX); // mov [edi+4],ebx
        e.shr_r32_imm8(Reg::R10, 24); // shr edx,24
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx]
        e.add_r64_imm32(Reg::R13, STRIDE); // add edi,STRIDE (host ptr)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx]
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec count (sets ZF)
        e.load_r32_sib(Reg::R11, Reg::R12, Reg::R9); // mov eax,[esi+ecx] (no flag change)
        e.load_r32_sib(Reg::RBX, Reg::R12, Reg::R10); // mov ebx,[esi+edx] (no flag change)
        e.jnz(top); // jnz entry
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed on the dev host")
    };
    type NativeFn = unsafe extern "C" fn(*mut u8, u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        // interpreter: run_straight_line chains a bounded number of instructions per call then
        // returns (a non-continuable insn / the final HLT ends the run), exactly as under the
        // machine. Drive it until the guest loop counter reaches 0.
        let mut cpu = Cpu386::default();
        setup(&mut cpu);
        let mut bus = TestBus::with_memory(build_mem());
        // TestBus defaults to Full tracing (an unbounded per-access cycle Vec) — a test
        // instrumentation cost the real MachineBus never pays. Disable it so the interpreter
        // baseline is representative. (Residual caveat: TestBus still lacks MachineBus's cached
        // raw-pointer direct-page path, so it is marginally slower than the real bus — a small
        // bias in the native side's favor, noted in the results.)
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let count_of = |bus: &TestBus| {
            u32::from_le_bytes(
                bus.memory[COUNT_ADDR as usize..COUNT_ADDR as usize + 4]
                    .try_into()
                    .unwrap(),
            )
        };
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || count_of(&bus) == 0 {
                break;
            }
            assert!(
                calls < ITERS as u64 * 4 + 1000,
                "run_straight_line not converging (calls={calls}, left={})",
                count_of(&bus)
            );
        }
        let ins = t.elapsed().as_secs_f64();
        let left = count_of(&bus);
        assert_eq!(
            left, 0,
            "loop did not run all {ITERS} iterations (count={left})"
        );
        if trial == 0 {
            eprintln!(
                "(interp chaining: {calls} run_straight_line calls for {ITERS} iters = {:.1} insns/call)",
                insns as f64 / calls as f64
            );
        }

        // native (identical fresh memory)
        let mut memn = build_mem();
        let t = Instant::now();
        unsafe { native_fn(memn.as_mut_ptr(), ITERS) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: framebuffers must be byte-identical
        let a = &bus.memory[FB as usize..FB as usize + FB_LEN];
        let b = &memn[FB as usize..FB as usize + FB_LEN];
        if a != b {
            let idx = a.iter().zip(b).position(|(x, y)| x != y).unwrap();
            panic!(
                "native framebuffer diverges from interpreter at FB+{idx}: interp={} native={}",
                a[idx], b[idx]
            );
        }

        if trial > 0 {
            // discard trial 0 (cold host caches)
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== G0' CPU-ceiling probe ({ITERS} iters x 15 insns, median of {TRIALS}) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native (best-case, reg-allocated + raw-ptr mem) : {mn:.3} ns/guest-insn");
    eprintln!(
        "SPEEDUP CEILING : {:.2}x   [4x = 'already very good' bar]",
        mi / mn
    );
    eprintln!("=== end G0' (memory-mixed) ===\n");
}

/// G0' companion: an ARTIFACT-FREE dispatch-only ceiling. A 15-instruction register-only loop
/// (no memory operands at all, so ZERO bus involvement — the TestBus memory-path caveat cannot
/// apply) isolating the interpreter's pure per-instruction dispatch/decode/flag/clock-accounting
/// overhead vs native register ops. This anchors that the memory-mixed interpreter figure is real
/// and not a bus artifact. Correctness is checked by comparing final guest register values.
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn g0_prime_dispatch_ceiling_probe() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    const STEP1: u32 = 0x0134_5677;
    const STEP2: u32 = 0x0023_4561;
    const EBP0: u32 = 0x1234_5678;
    const ITERS: u32 = 300_000;
    const TRIALS: usize = 7;

    // register-only guest loop (counter in edi); trailing HLT.
    let mut code: Vec<u8> = Vec::new();
    code.extend_from_slice(&[0x8B, 0xCD]); // mov ecx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add ebp,STEP1
    code.extend_from_slice(&[0xC1, 0xE9, 0x18]); // shr ecx,24
    code.extend_from_slice(&[0x8B, 0xD5]); // mov edx,ebp
    code.extend_from_slice(&[0x81, 0xC5]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebp,STEP2
    code.extend_from_slice(&[0xC1, 0xEA, 0x18]); // shr edx,24
    code.extend_from_slice(&[0x8B, 0xC1]); // mov eax,ecx
    code.extend_from_slice(&[0x81, 0xC0]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add eax,STEP1
    code.extend_from_slice(&[0xC1, 0xE8, 0x03]); // shr eax,3
    code.extend_from_slice(&[0x8B, 0xDA]); // mov ebx,edx
    code.extend_from_slice(&[0x81, 0xC3]);
    code.extend_from_slice(&STEP2.to_le_bytes()); // add ebx,STEP2
    code.extend_from_slice(&[0xC1, 0xEB, 0x03]); // shr ebx,3
    code.extend_from_slice(&[0x81, 0xC6]);
    code.extend_from_slice(&STEP1.to_le_bytes()); // add esi,STEP1
    code.extend_from_slice(&[0x81, 0xC7]);
    code.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // add edi,-1 (dec, sets ZF)
    let jnz_at = code.len();
    let rel = (0i32 - (jnz_at as i32 + 2)) as i8;
    code.extend_from_slice(&[0x75, rel as u8]); // jnz entry
    code.push(0xF4); // hlt

    let seg = |selector: u16, access: u8| SegmentRegister {
        selector,
        base: 0,
        limit: 0xffff_ffff,
        access,
        default_size_32: true,
    };

    // native register-only emission; final regs written to out[0..6] = eax,ebx,ecx,edx,ebp,esi.
    let native = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R12);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r32_r32(Reg::R14, Reg::RCX); // count = iters (arg0)
        e.mov_r64_r64(Reg::R15, Reg::RDX); // out ptr (arg1)
        e.mov_r32_imm32(Reg::R8, EBP0); // ebp
        e.mov_r32_imm32(Reg::R9, 0); // ecx
        e.mov_r32_imm32(Reg::R10, 0); // edx
        e.mov_r32_imm32(Reg::R11, 0); // eax
        e.mov_r32_imm32(Reg::RBX, 0); // ebx
        e.mov_r32_imm32(Reg::R12, 0); // esi
        let top = e.label();
        e.place(top);
        e.mov_r32_r32(Reg::R9, Reg::R8); // mov ecx,ebp
        e.add_r32_imm32(Reg::R8, STEP1);
        e.shr_r32_imm8(Reg::R9, 24);
        e.mov_r32_r32(Reg::R10, Reg::R8); // mov edx,ebp
        e.add_r32_imm32(Reg::R8, STEP2);
        e.shr_r32_imm8(Reg::R10, 24);
        e.mov_r32_r32(Reg::R11, Reg::R9); // mov eax,ecx
        e.add_r32_imm32(Reg::R11, STEP1);
        e.shr_r32_imm8(Reg::R11, 3);
        e.mov_r32_r32(Reg::RBX, Reg::R10); // mov ebx,edx
        e.add_r32_imm32(Reg::RBX, STEP2);
        e.shr_r32_imm8(Reg::RBX, 3);
        e.add_r32_imm32(Reg::R12, STEP1); // add esi,STEP1
        e.add_r32_imm32(Reg::R14, 0xFFFF_FFFF); // dec edi (counter), sets ZF
        e.jnz(top);
        e.store_r32_disp8(Reg::R15, 0, Reg::R11); // out[0]=eax
        e.store_r32_disp8(Reg::R15, 4, Reg::RBX); // out[1]=ebx
        e.store_r32_disp8(Reg::R15, 8, Reg::R9); // out[2]=ecx
        e.store_r32_disp8(Reg::R15, 12, Reg::R10); // out[3]=edx
        e.store_r32_disp8(Reg::R15, 16, Reg::R8); // out[4]=ebp
        e.store_r32_disp8(Reg::R15, 20, Reg::R12); // out[5]=esi
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R12);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X alloc must succeed")
    };
    type NativeFn = unsafe extern "C" fn(u32, *mut u32);
    let native_fn: NativeFn = unsafe { std::mem::transmute(native.entry_ptr()) };

    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let insns = 15u64 * ITERS as u64;
    let mut interp_ns = Vec::new();
    let mut native_ns = Vec::new();
    for trial in 0..=TRIALS {
        let mut cpu = Cpu386::default();
        cpu.control.cr0 |= CR0_PE;
        cpu.registers.set_segment(SegmentIndex::Cs, seg(0x08, 0x9b));
        cpu.registers.set_segment(SegmentIndex::Ds, seg(0x10, 0x93));
        cpu.registers.set_segment(SegmentIndex::Ss, seg(0x10, 0x93));
        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edx(0);
        cpu.registers.set_ebp(EBP0);
        cpu.registers.set_esi(0);
        cpu.registers.set_edi(ITERS);
        let mut mem = vec![0u8; 0x1000];
        mem[..code.len()].copy_from_slice(&code);
        let mut bus = TestBus::with_memory(mem);
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        let t = Instant::now();
        let mut calls = 0u64;
        loop {
            let out = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
            calls += 1;
            if out.halted || cpu.registers.edi() == 0 {
                break;
            }
            assert!(calls < ITERS as u64 * 4 + 1000, "not converging");
        }
        let ins = t.elapsed().as_secs_f64();
        assert_eq!(cpu.registers.edi(), 0, "loop did not complete");

        let mut out = [0u32; 6];
        let t = Instant::now();
        unsafe { native_fn(ITERS, out.as_mut_ptr()) };
        let nns = t.elapsed().as_secs_f64();

        // correctness: final registers must match.
        let interp_regs = [
            cpu.registers.eax(),
            cpu.registers.ebx(),
            cpu.registers.ecx(),
            cpu.registers.edx(),
            cpu.registers.ebp(),
            cpu.registers.esi(),
        ];
        assert_eq!(
            out, interp_regs,
            "native register-only result diverges from interpreter"
        );

        if trial > 0 {
            interp_ns.push(ins / insns as f64 * 1e9);
            native_ns.push(nns / insns as f64 * 1e9);
        }
    }
    let mi = median(interp_ns);
    let mn = median(native_ns);
    eprintln!("\n=== G0' dispatch-only ceiling (register-only loop, NO memory/bus artifact) ===");
    eprintln!("interpreter : {mi:.3} ns/guest-insn");
    eprintln!("native      : {mn:.3} ns/guest-insn");
    eprintln!("DISPATCH SPEEDUP CEILING : {:.2}x", mi / mn);
    eprintln!("=== end G0' (dispatch-only) ===\n");
}

/// S2.2 spike (owner chose "spike first"): the reg-cache alone is not the lever; the per-slot
/// fetch/cap CALLs back into Rust are the floor (the region is wall-neutral with the interpreter
/// precisely because native emitted code cannot inline the bus/cpu work the Rust trampoline
/// inlines). This measures the per-slot BOOKKEEPING cost (fetch charge + clock accumulate +
/// cross-multiplied cap check) under the three candidate models, the variable that decides the
/// S2 build. Combined with the G0' compute (~0.38 ns/insn dirty native) and interpreter
/// (~96 ns/insn) numbers, it gives the drawcolumn per-insn estimate for each model. Throwaway.
///   cargo test -j8 -p izarravm-cpu --release --features jit s2_bookkeeping -- --ignored --nocapture
#[cfg(feature = "jit")]
#[test]
#[ignore]
fn s2_bookkeeping_model_spike() {
    use crate::jit::encoder::{Encoder, Reg};
    use crate::jit::exec_mem::ExecutableBuffer;
    use std::time::Instant;

    #[repr(C)]
    struct SpikeState {
        raw_clocks: u64, // off 0
        bus_accum: u64,  // off 8
        cap: u64,        // off 16
        num: u64,        // off 24
    }
    const FETCH: u32 = 2; // representative RAM I-cache fetch wait-state (machine/lib.rs:9799)
    const NUM: u32 = 1; // 586 timing numerator

    // One slot's realistic bookkeeping: fetch charge + clock accumulate + cross-mult cap check.
    unsafe extern "C" fn book_one(s: *mut SpikeState) -> u8 {
        let s = unsafe { &mut *s };
        s.bus_accum += FETCH as u64;
        s.raw_clocks += 2;
        u8::from(s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap)
    }
    // The same, batched: n slots in one call (amortizes the CALL over the block iteration).
    unsafe extern "C" fn book_batch(s: *mut SpikeState, n: u32) -> u8 {
        let s = unsafe { &mut *s };
        for _ in 0..n {
            s.bus_accum += FETCH as u64;
            s.raw_clocks += 2;
            if s.raw_clocks.wrapping_mul(s.num).wrapping_add(s.bus_accum) >= s.cap {
                return 1;
            }
        }
        0
    }
    let one_addr = (book_one as unsafe extern "C" fn(*mut SpikeState) -> u8) as usize as u64;
    let batch_addr =
        (book_batch as unsafe extern "C" fn(*mut SpikeState, u32) -> u8) as usize as u64;

    // Model 1 (today's region): one Rust bookkeeping CALL per slot. win64 arg0=RCX(state).
    let call_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32); // shadow space; RSP 16-aligned before the CALL
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.mov_r64_imm64(Reg::RBX, one_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF); // dec (sets ZF)
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 2 (Option A): bookkeeping inline, accumulators cached in host registers (no CALL).
    let native_per_slot = {
        let mut e = Encoder::new();
        e.push(Reg::RSI);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters
        e.load_r64_disp8(Reg::R12, Reg::R15, 0); // raw_clocks
        e.load_r64_disp8(Reg::R13, Reg::R15, 8); // bus_accum
        e.load_r64_disp8(Reg::RSI, Reg::R15, 16); // cap
        let top = e.label();
        let exit = e.label();
        e.place(top);
        e.add_r64_imm32(Reg::R12, 2); // raw += 2
        e.add_r64_imm32(Reg::R13, FETCH); // bus += fetch cost
        e.mov_r64_r64(Reg::RAX, Reg::R12);
        e.imul_r64_imm32(Reg::RAX, NUM); // raw * num
        e.add_r64_r64(Reg::RAX, Reg::R13); // + bus term
        e.cmp_r64_r64(Reg::RAX, Reg::RSI); // vs cap
        e.jae(exit);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.place(exit);
        e.store_r64_disp8(Reg::R15, 0, Reg::R12);
        e.store_r64_disp8(Reg::R15, 8, Reg::R13);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RSI);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    // Model 3 (Option B): one Rust CALL per block-iteration doing n slots' bookkeeping.
    let batched = {
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::R14);
        e.push(Reg::R15);
        e.sub_r64_imm32(Reg::RSP, 32);
        e.mov_r64_r64(Reg::R15, Reg::RCX); // state
        e.mov_r64_r64(Reg::R14, Reg::RDX); // iters (= SLOTS / 15)
        e.mov_r64_imm64(Reg::RBX, batch_addr);
        let top = e.label();
        e.place(top);
        e.mov_r64_r64(Reg::RCX, Reg::R15);
        e.mov_r32_imm32(Reg::RDX, 15); // n slots / iteration
        e.call_r64(Reg::RBX);
        e.add_r64_imm32(Reg::R14, 0xFFFF_FFFF);
        e.jnz(top);
        e.add_r64_imm32(Reg::RSP, 32);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::RBX);
        e.ret();
        ExecutableBuffer::new(&e.finish()).expect("W^X on a supported host")
    };

    type Fn2 = unsafe extern "C" fn(*mut SpikeState, u64);
    const SLOTS: u64 = 15_000_000;
    const TRIALS: usize = 7;
    let run = |buf: &ExecutableBuffer, iters: u64| -> f64 {
        let f: Fn2 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        let mut st = SpikeState {
            raw_clocks: 0,
            bus_accum: 0,
            cap: u64::MAX, // never fires: every model processes all SLOTS
            num: NUM as u64,
        };
        let t = Instant::now();
        unsafe { f(&mut st, iters) };
        t.elapsed().as_secs_f64() / SLOTS as f64 * 1e9 // ns per slot
    };
    let median = |mut v: Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v[v.len() / 2]
    };
    let (mut c, mut n, mut b) = (Vec::new(), Vec::new(), Vec::new());
    for trial in 0..=TRIALS {
        let tc = run(&call_per_slot, SLOTS);
        let tn = run(&native_per_slot, SLOTS);
        let tb = run(&batched, SLOTS / 15);
        if trial > 0 {
            c.push(tc);
            n.push(tn);
            b.push(tb);
        }
    }
    let (mc, mn, mb) = (median(c), median(n), median(b));
    eprintln!("\n=== S2.2 bookkeeping-model spike (ns per slot, median of {TRIALS}) ===");
    eprintln!("1. call-per-slot  (today's region model)     : {mc:.3} ns/slot");
    eprintln!("2. native-per-slot (Option A, cached accum)  : {mn:.3} ns/slot");
    eprintln!("3. batched CALL/iter (Option B, 15 slots)    : {mb:.3} ns/slot");
    eprintln!("--- drawcolumn per-insn estimate = ~0.38 ns native compute + bookkeeping/slot ---");
    eprintln!("current region (wall-neutral w/ interp)      : ~96 ns/insn (G0')");
    eprintln!(
        "Option A (native per-slot)  : {:.2} ns/insn  => {:.0}x over current",
        0.38 + mn,
        96.0 / (0.38 + mn)
    );
    eprintln!(
        "Option B (batched)          : {:.2} ns/insn  => {:.0}x over current  (+ omitted spill/reload)",
        0.38 + mb,
        96.0 / (0.38 + mb)
    );
    eprintln!("=== end S2.2 spike ===\n");
}

#[test]
fn scale_clocks_batches_exactly() {
    // The JIT accumulates raw core_clocks across a straight-line block and scales ONCE at
    // block exit. That is bit-identical to per-instruction scaling because scale_clocks is
    // exact long division with a remainder carry. Verified across every mode, several clock
    // sequences, and a non-zero starting remainder. A regression here silently breaks the
    // JIT's cyc/iter identity, so this guards the property.
    let seqs: [&[u32]; 3] = [
        &[3, 5, 1, 1, 61, 2, 7, 4, 9, 2],
        &[1; 32],
        &[255, 1, 100, 3, 17, 61, 61, 2],
    ];
    for level in [
        CpuLevel::I286,
        CpuLevel::I386,
        CpuLevel::I486,
        CpuLevel::I586,
    ] {
        for start_rem in [0u64, 1, 7, 100] {
            for seq in seqs {
                let mut indiv = Cpu386::default();
                indiv.set_level(level);
                indiv.timing_rem = start_rem;
                let mut batch = Cpu386::default();
                batch.set_level(level);
                batch.timing_rem = start_rem;

                let sum_individual: u64 = seq.iter().map(|&c| indiv.scale_clocks(c)).sum();
                let total: u32 = seq.iter().sum();
                let batched = batch.scale_clocks(total);

                assert_eq!(
                    sum_individual, batched,
                    "level {level:?} rem {start_rem} seq {seq:?}: per-insn sum != batched"
                );
                assert_eq!(
                    indiv.timing_rem, batch.timing_rem,
                    "level {level:?} rem {start_rem} seq {seq:?}: remainder carry diverged"
                );
            }
        }
    }
}

/// S0 (JIT/perf plan, council #1 finding): a compiled block that contains x87 ops carries a
/// SECOND remainder (`fp_rem`) besides the integer `timing_rem`, and it must batch into one
/// block-exit flush with the SAME carry as per-op scaling, or the block's guest cycle count
/// diverges from the interpreter. Unlike integer clocks (one per-level numerator), FP ops have
/// PER-CLASS numerators, so the batched form weights each op by its class before summing; the
/// shared `FP_TIMING_DEN` is what keeps the single `fp_rem` carry exact across mixed classes.
/// This pins `Σ scale_fp_clocks == floor((Σ clocks·num_class + rem0) / DEN)` with the final
/// remainder `(Σ clocks·num_class + rem0) % DEN` — the identity a future `scale_fp_clocks_batch`
/// (added with S2's x87 templates) must satisfy, and a guard that scale_fp_clocks never drops
/// the shared-denominator property the batch relies on.
#[test]
fn scale_fp_clocks_batches_exactly() {
    use FpOpClass::{F32Mem, F64Mem, IntConvert16, IntConvert32, Register, Wait};
    let seqs: [&[(u32, FpOpClass)]; 3] = [
        &[
            (4, IntConvert32),
            (1, Register),
            (3, F64Mem),
            (2, IntConvert16),
            (1, Register),
        ],
        &[(1, Register); 20],
        &[
            (7, F32Mem),
            (2, IntConvert32),
            (9, Register),
            (1, Wait),
            (5, IntConvert16),
            (3, F64Mem),
        ],
    ];
    for level in [
        CpuLevel::I286,
        CpuLevel::I386,
        CpuLevel::I486,
        CpuLevel::I586,
    ] {
        for start_rem in [0u64, 1, 5, 7] {
            for seq in seqs {
                let mut indiv = Cpu386::default();
                indiv.set_level(level);
                indiv.fp_rem = start_rem;
                let sum_individual: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(indiv.scale_fp_clocks(c, cl)))
                    .sum();

                // Closed-form batched value: sum the per-op class-weighted numerators, then one
                // exact division with the single carried remainder.
                let weighted: u64 = seq
                    .iter()
                    .map(|&(c, cl)| u64::from(c) * u64::from(fp_timing_class(level, cl)))
                    .sum();
                let scaled = weighted + start_rem;
                let batched = scaled / u64::from(FP_TIMING_DEN);
                let final_rem = scaled % u64::from(FP_TIMING_DEN);

                assert_eq!(
                    sum_individual, batched,
                    "level {level:?} rem {start_rem}: per-op FP sum != batched"
                );
                assert_eq!(
                    indiv.fp_rem, final_rem,
                    "level {level:?} rem {start_rem}: fp_rem carry diverged"
                );
            }
        }
    }
}

#[derive(Default)]
struct TestBus {
    memory: Vec<u8>,
    trace: BusTrace,
    pending_irq: Option<u8>,
    // Mirrors the machine's `io_touched`: set by any port access, so `requires_step_break`
    // reports the same step-break edge the real bus does.
    io_touched: bool,
    // When true, `read_io` does NOT set `io_touched`, modeling the machine's
    // Approximate-class lazy status-port path (MachineBus::read_io's
    // 3DA/3BA/3C2 arm), so poll-loop chaining across an IN can be exercised
    // through the CPU alone. Writes still set io_touched (no lazy write path
    // exists on the machine either). Default false: the classic every-port-
    // access-breaks behavior.
    lazy_io_reads: bool,
    // Records the `core_clocks_so_far` value the CPU threaded into the most recent
    // `read_io` call, so tests can assert on it directly (see
    // `core_clocks_so_far_reflects_prior_instructions_not_the_in_flight_one`).
    last_read_io_core_clocks_so_far: Option<u64>,
    // When true, `direct_page` hands out host-pointer pages into `memory` (mirroring the
    // production MachineBus), so data accesses take the CPU's cached host-pointer deref path
    // instead of the slow `read_memory_direct` fallback. Default false: the historical
    // no-direct-page behavior every existing test relies on (data accesses push trace cycles).
    // The JIT memory microbenchmark sets it true so its numbers reflect production, not the
    // slow test path (which does not exist on the real bus).
    direct_pages_enabled: bool,
}

impl TestBus {
    fn with_memory(memory: Vec<u8>) -> Self {
        Self {
            memory,
            trace: BusTrace::default(),
            pending_irq: None,
            io_touched: false,
            lazy_io_reads: false,
            last_read_io_core_clocks_so_far: None,
            direct_pages_enabled: false,
        }
    }
}

impl CpuBus for TestBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        Ok(match width {
            BusWidth::Byte => u32::from(self.memory[start]),
            BusWidth::Word => u32::from(u16::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
            ])),
            BusWidth::Dword => u32::from_le_bytes([
                self.memory[start],
                self.memory[start + 1],
                self.memory[start + 2],
                self.memory[start + 3],
            ]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(kind, address, width, 0));
        let start = address as usize;
        let end = start
            .checked_add(width.bytes() as usize)
            .ok_or(BusError::UnmappedMemory { address })?;
        if end > self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        match width {
            BusWidth::Byte => self.memory[start] = value as u8,
            BusWidth::Word => {
                self.memory[start..start + 2].copy_from_slice(&(value as u16).to_le_bytes())
            }
            BusWidth::Dword => self.memory[start..start + 4].copy_from_slice(&value.to_le_bytes()),
        }
        Ok(())
    }

    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width)
            == width.bytes() as usize
        {
            return self
                .read_memory(address, width, kind)
                .map(|value| DirectMemoryRead {
                    value,
                    direct: true,
                });
        }
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        if self.direct_memory_bytes(address, width.bytes() as usize, width)
            == width.bytes() as usize
        {
            self.write_memory(address, width, value, kind)?;
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)?;
        Ok(DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if self.direct_memory_bytes(address, out.len(), width) != out.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..out.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        out.copy_from_slice(&self.memory[start..start + out.len()]);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if self.direct_memory_bytes(address, data.len(), width) != data.len() {
            return Ok(0);
        }
        let access = width.bytes() as usize;
        for offset in (0..data.len()).step_by(access) {
            self.trace
                .push(BusCycle::new(kind, address + offset as u32, width, 0));
        }
        let start = address as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn direct_memory_bytes(&self, address: u32, bytes: usize, width: BusWidth) -> usize {
        if bytes == 0 || (address as usize & 0x0fff) + bytes > 0x1000 {
            return 0;
        }
        if matches!(width, BusWidth::Word) && address & 1 != 0
            || matches!(width, BusWidth::Dword) && address & 3 != 0
        {
            return 0;
        }
        let start = address as usize;
        if start
            .checked_add(bytes)
            .is_some_and(|end| end <= self.memory.len())
        {
            bytes
        } else {
            0
        }
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let start = address as usize;
        if start >= self.memory.len() {
            return Err(BusError::UnmappedMemory { address });
        }
        let len = out.len().min(self.memory.len() - start);
        out[..len].copy_from_slice(&self.memory[start..start + len]);
        Ok(len)
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    // The trait default charges a fetch run byte-by-byte (one cross-crate call + push per
    // byte), whose call overhead dominates JIT-region wall-clock microbenchmarks. This override
    // is bit-identical to that default loop in EVERY accounting field (clocks, access count,
    // Full-mode detail) but does it in one op, so no existing test changes. It does NOT
    // reproduce the production MachineBus, which collapses a cacheable-RAM run to ONE access at
    // the code-fetch wait state (this keeps `count` byte accesses at wait-state 0); the
    // microbenchmark runs tracing Off, so it measures wall clock, not the fetch-clock total.
    // Do not treat this TestBus's instruction-fetch clock accounting as production-representative.
    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        self.trace.record_instruction_fetch_run(start, count, 0);
        Ok(())
    }

    // Hand out a host-pointer page into `memory`, mirroring MachineBus::direct_page, so the
    // CPU's data_read_pages/data_write_pages caches populate and subsequent accesses are host
    // derefs. Gated: off by default (the default trait None keeps every existing test on the
    // slow read_memory_direct path with its trace cycles), on only for the JIT microbenchmark.
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        if !self.direct_pages_enabled {
            return Ok(None);
        }
        let physical_page = address & !0x0fff;
        let start = physical_page as usize;
        if start + 0x1000 > self.memory.len() {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: 0x1000,
            writable: matches!(kind, BusAccessKind::DataWrite),
        }))
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        _cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        if !self.lazy_io_reads {
            self.io_touched = true;
        }
        self.last_read_io_core_clocks_so_far = Some(core_clocks_so_far);
        self.trace.push(BusCycle::new(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            0,
        ));
        Ok(0)
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        _value: u32,
        _cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.io_touched = true;
        self.trace.push(BusCycle::new(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            0,
        ));
        Ok(())
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.push(BusCycle::new(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            0,
        ));
        Ok(())
    }

    fn interrupt_pending(&self) -> bool {
        self.pending_irq.is_some()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pending_irq.take()
    }

    fn requires_step_break(&self) -> bool {
        self.io_touched
    }
}

#[test]
fn reset_state_starts_at_386_reset_vector() {
    let cpu = Cpu386::default();

    assert_eq!(cpu.registers.cs().selector, 0xf000);
    assert_eq!(cpu.registers.cs().base, 0xffff_0000);
    assert_eq!(cpu.registers.eip, 0xfff0);
    assert_eq!(cpu.linear_eip(), 0xffff_fff0);
}

#[test]
fn core_clocks_so_far_is_zero_for_an_in_as_the_runs_first_instruction_in_the_accurate_class() {
    // In the Accurate class (I286/I386) `block_continuable` never admits
    // `DecodeGroup::PortIo` (see that function's doc comment: the P4a Task 1.3
    // IN admission is gated on the Approximate class only), so an IN can ONLY
    // ever be `run_straight_line`'s FIRST instruction there, never a
    // continuation -- every port access still sets `io_touched` unconditionally
    // in the Accurate class's read_io dispatch, ending the run right after it
    // runs. This test pins core_clocks_so_far == 0 for that first-instruction
    // position, explicitly on I386 so it does not silently start exercising the
    // Approximate-class continuation path if the CPU's default level ever
    // changes. See the sibling test for the Approximate-class continuation case.
    let code = [0xec]; // in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_level(CpuLevel::I386);
    let mut bus = TestBus::with_memory(memory);

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        bus.last_read_io_core_clocks_so_far,
        Some(0),
        "an IN that is the run's first (and, in the Accurate class, only \
             possible) instruction position sees core_clocks_so_far == 0"
    );
    assert!(
        outcome.core_clocks > 0,
        "the IN itself still charges clocks"
    );
}

#[test]
fn core_clocks_so_far_tracks_the_running_total_for_an_in_reached_as_an_approximate_class_continuation()
 {
    // P4a Task 1.3: in the Approximate class (I486/I586) `block_continuable`
    // admits the IN forms (0xe4/0xe5/0xec/0xed), so an IN reached as a
    // continuation (not the run's first instruction) must see
    // core_clocks_so_far equal to the running total of every prior
    // instruction in the run, exactly like the Group/DataMove continuation
    // case pinned in `core_clocks_so_far_tracks_run_straight_lines_total_before_each_continuation`.
    // Eight INCs then an IN: the IN's core_clocks_so_far must equal the eight
    // INCs' combined charge. Eight (not two, unlike the sibling Accurate-class
    // test) because I586's `level_timing` factor is (1, 12) -- a single cheap
    // INC can legitimately round to 0 charged clocks under the fractional
    // remainder carry (see `scale_clocks`'s doc comment), so a short run risks
    // a degenerate all-zero total that cannot distinguish "tracks the running
    // total" from "always reads 0". Eight instructions guarantees the carry
    // has produced a nonzero total well before the IN.
    let code = [0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0xec]; // inc ax x8; in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    // real_mode_cpu's default level (Cpu386::default()) is already I586
    // (Approximate); set it explicitly so this test does not silently change
    // meaning if the default ever moves.
    cpu.set_level(CpuLevel::I586);
    let mut bus = TestBus::with_memory(memory);
    // Warm the decode cache one instruction at a time via single-step `cycle`
    // (not `run_straight_line`): once the IN is continuable, a warm-up call
    // to `run_straight_line` may itself chain multiple instructions per call,
    // so the number of `run_straight_line` calls needed to warm exactly 9
    // addresses is no longer deterministic. `cycle` always decodes and
    // advances exactly one instruction per call, so 9 calls warms exactly
    // addresses 0..9 regardless of continuability.
    for _ in 0..9 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.reset_perf_counters();
    // The warm-up's IN (address 8) set TestBus::io_touched, which never
    // self-clears (unlike the real machine batch loop, which opens each batch
    // with a fresh false). Clear it here so the measurement run below is not
    // ended by stale warm-up state on its very first instruction.
    bus.io_touched = false;

    // Independently capture "the eight INCs' combined charge" the same way
    // the sibling Group-continuation test does: clone the warmed-up CPU (so
    // its `timing_rem` fractional-clock carry matches) and single-step eight
    // INCs on a clone bus.
    let eight_incs_total = {
        let mut solo = cpu.clone();
        let mut solo_bus = TestBus::with_memory(vec![0x40; 8]);
        let mut total = 0u32;
        for _ in 0..8 {
            total += solo.cycle(&mut solo_bus).unwrap().core_clocks;
        }
        total
    };
    assert!(
        eight_incs_total > 0,
        "sanity: eight INCs must have produced a nonzero charge under the \
             remainder carry, or this test cannot distinguish the running total \
             from a degenerate always-0 read"
    );

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        cpu.perf_counters().straight_line_runs,
        1,
        "one chained run: eight INCs then the IN, all continuable in the \
             Approximate class"
    );
    assert_eq!(
        bus.last_read_io_core_clocks_so_far,
        Some(eight_incs_total.into()),
        "the IN reached as the run's ninth instruction (a continuation) must \
             see core_clocks_so_far equal to the eight INCs' combined charge, not 0"
    );
    assert!(
        outcome.core_clocks > eight_incs_total,
        "the IN's own charge must be included in the run total"
    );
}

#[test]
fn poll_loop_with_test_imm_chains_end_to_end_in_the_approximate_class() {
    // The canonical vretrace poll idiom: IN; TEST AL,imm8; JZ back; (JMP back,
    // unreachable here since AL reads 0 so ZF is always set). With 0xa8
    // admitted alongside the IN forms in the Approximate class, the WHOLE
    // loop must chain as one run_straight_line call up to the clock cap --
    // no run restart per iteration. The bus models the machine's lazy
    // status-port path (lazy_io_reads: reads do not set io_touched), since
    // chaining across the IN is only reachable when the port read is lazy.
    let code = [
        0xEC, // 0: in al, dx (TestBus returns 0 -> AL = 0)
        0xA8, 0x08, // 1: test al, 0x08 (AL=0 -> ZF set)
        0x74, 0xFB, // 3: jz -5 -> back to 0 (always taken)
        0xEB, 0xF9, // 5: jmp -7 -> back to 0 (unreachable, decode fodder only)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_level(CpuLevel::I586);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    // Warm the decode cache: one single-step per loop instruction (IN, TEST,
    // JZ -- the JMP is unreachable and irrelevant to the chain).
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.eip, 0, "warm-up looped back to the IN");
    cpu.reset_perf_counters();

    // A finite cap: the loop never exits on its own, so the ONLY clean end
    // for a fully-chained run is the cap. Big enough for many iterations.
    let outcome = cpu.run_straight_line(&mut bus, 1_000).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "the whole poll loop must chain inside ONE run_straight_line call"
    );
    assert_eq!(
        p.brk_cap, 1,
        "the run must end on the clock cap, not on a step break or a \
             non-continuable terminator (brk_step={}, brk_branch={})",
        p.brk_step, p.brk_decode_or_branch
    );
    assert!(
        p.instructions > 100,
        "hundreds of poll iterations must fit under the cap once the loop \
             chains (saw {} instructions)",
        p.instructions
    );
    assert!(
        bus.last_read_io_core_clocks_so_far.unwrap() > 0,
        "a late-iteration IN reached as a continuation must see the running \
             (nonzero) core-clock total, proving the INs chained mid-run"
    );
    assert!(
        u64::from(outcome.core_clocks) >= 1_000,
        "the chained run must have consumed the whole cap"
    );
}

#[test]
fn poll_loop_test_imm_still_terminates_the_run_in_the_accurate_class() {
    // The complementary Accurate-class pin: at I386 neither the IN (0xec)
    // nor the TEST (0xa8) is continuable, so even with the bus's lazy-read
    // knob on (no io_touched step break at all), the same poll loop must
    // stop at the first continuation attempt: the run is exactly the one IN,
    // ended by TEST's non-admission. This is the byte-identical run-shape
    // guarantee for 286/386.
    let code = [
        0xEC, // 0: in al, dx
        0xA8, 0x08, // 1: test al, 0x08
        0x74, 0xFB, // 3: jz -5 -> back to 0
        0xEB, 0xF9, // 5: jmp -7 -> back to 0
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_level(CpuLevel::I386);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.eip, 0, "warm-up looped back to the IN");
    cpu.reset_perf_counters();

    let _ = cpu.run_straight_line(&mut bus, 1_000).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one run_straight_line call was made"
    );
    assert_eq!(
        p.instructions, 1,
        "the Accurate class must retire exactly the IN and stop at the \
             non-continuable TEST (no io_touched break was available to end it, \
             so this pins the admission gate itself)"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "the run must end on the continuation-admission check, not a step \
             break (brk_step={})",
        p.brk_step
    );
}

#[test]
fn in_stays_a_run_terminator_not_a_continuation_in_the_accurate_class() {
    // Pins the IN half of the Approximate-class admission gate, which the
    // sibling poll-loop test cannot: there the run ends at the TEST before
    // any continuation attempt ever reaches an IN, so deleting the level
    // gate from the PortIo arm alone would not fail it (the spec review
    // proved the earlier Accurate-class test -- a single IN at eip 0,
    // trivially the run's first instruction -- pinned nothing). Here two
    // continuable INCs precede the IN: at I386 the run must chain the INCs
    // and stop at the continuation-admission check BEFORE the IN executes,
    // observable as read_io never having been called during the run. The
    // bus's lazy-read knob is on, so no io_touched step break could end the
    // run in the gate's place. Mutation-verified: with the level gate
    // removed from the PortIo arm the IN chains and read_io fires, failing
    // the None assertion; with the gate intact it passes.
    let code = [0x40, 0x40, 0xec]; // inc ax; inc ax; in al,dx
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_level(CpuLevel::I386);
    let mut bus = TestBus::with_memory(memory);
    bus.lazy_io_reads = true;
    // Warm all three decode-cache lines via single-steps (the IN included,
    // so its cached `continuable` flag is what gates the measured run).
    for _ in 0..3 {
        let _ = cpu.cycle(&mut bus).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.reset_perf_counters();
    // The warm-up executed the IN once; clear its trace so the assertion
    // below observes only the measured run.
    bus.last_read_io_core_clocks_so_far = None;

    let _ = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    let p = cpu.perf_counters();
    assert_eq!(
        p.instructions, 2,
        "the Accurate class must retire exactly the two INCs and stop at \
             the non-continuable IN"
    );
    assert_eq!(
        bus.last_read_io_core_clocks_so_far, None,
        "read_io must NOT have been called: the run stopped BEFORE the IN, \
             at the continuation-admission check"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "the run must end on the continuation-admission check, not a step \
             break (brk_step={})",
        p.brk_step
    );
}

#[test]
fn core_clocks_so_far_tracks_run_straight_lines_total_before_each_continuation() {
    // Directly pins the mechanism Task 0.2 adds (a Cpu386 field set to
    // run_straight_line's running `total` before every continuation dispatch,
    // read by read_io) using a continuable instruction group (INC, DataMove/
    // Alu-adjacent -- specifically Group) as the observation point, since
    // PortIo itself cannot reach the continuation path (see the sibling test).
    // Two INCs then a third INC: after the run, core_clocks_so_far must equal
    // whatever `total` was immediately before the LAST instruction executed
    // (i.e. the first two INCs' combined charge), proving the field tracks
    // the running total across continuations, not just "always 0" by
    // accident of PortIo's continuability gate.
    let code = [0x40, 0x40, 0x40]; // inc ax; inc ax; inc ax
    let (mut cpu, memory) = real_mode_cpu(&code, 32);
    cpu.set_level(CpuLevel::I286);
    let mut bus = TestBus::with_memory(memory);
    // Warm the decode cache one instruction at a time (INC is continuable, so
    // once warm all three chain in a single run_straight_line call).
    for _ in 0..3 {
        let _ = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    }
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.reset_perf_counters();

    // Independently capture "the first two INCs' combined charge" by cloning
    // the CPU right here (so its warmed-up `timing_rem` fractional-clock
    // carry, accumulated over the 3 warm-up runs, matches exactly) and
    // driving the clone through two `cycle()` single-steps.
    let two_incs_total = {
        let mut solo = cpu.clone();
        let mut solo_bus = TestBus::with_memory(vec![0x40, 0x40]);
        let a = solo.cycle(&mut solo_bus).unwrap().core_clocks;
        let b = solo.cycle(&mut solo_bus).unwrap().core_clocks;
        a + b
    };

    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();

    assert_eq!(
        cpu.perf_counters().straight_line_runs,
        1,
        "one chained run: three INCs, no port access to break it early"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 3, "all three INCs retired");
    // core_clocks_so_far was set to `total` right before the run's LAST
    // continuation (the third INC) dispatched, so it must equal exactly the
    // first two INCs' combined charge, independently measured above.
    assert_eq!(cpu.core_clocks_so_far, u64::from(two_incs_total));
    assert!(
        u64::from(outcome.core_clocks) > u64::from(two_incs_total),
        "the third INC's own charge must be included in the run total"
    );
}

#[test]
fn register_aliasing_updates_low_parts() {
    let mut cpu = Cpu386::default();
    cpu.registers.set_eax(0x1234_5678);

    cpu.write_reg16(Reg16::Ax, 0xabcd);
    cpu.write_gpr8(4, 0xef);

    assert_eq!(cpu.registers.eax(), 0x1234_efcd);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xefcd);
}

#[test]
fn operand_prefix_allows_32bit_mov_in_real_mode() {
    let mut memory = vec![0; 32];
    memory[0..6].copy_from_slice(&[0x66, 0xb8, 0x78, 0x56, 0x34, 0x12]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x1234_5678);
    assert_eq!(cpu.registers.eip, 6);
}

#[test]
fn modrm_direct_address_can_store_ax() {
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x89, 0x06, 0x00, 0x02, 0xf4]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4f56);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x4f56
    );
}

#[test]
fn perf_counters_track_decode_hits_and_run_breaks() {
    // A tight loop: 0: inc ax (40); 1: inc ax (40); 2: jmp $-4 (EB FC) -> 0.
    let mut memory = vec![0u8; 1024];
    memory[0..4].copy_from_slice(&[0x40, 0x40, 0xeb, 0xfc]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    // Six single steps run the 3-instruction body twice (inc, inc, jmp).
    for _ in 0..6 {
        cpu.cycle(&mut bus).unwrap();
    }
    let p = cpu.perf_counters();
    assert_eq!(p.instructions, 6, "six instructions retired");
    // The three unique linear addresses decode once; the loop's second pass is
    // served from the decode cache, so misses stay at 3 (a 50% hit rate). This is
    // the assertion that fails if the decode cache (or the miss counter) breaks.
    assert_eq!(
        p.decode_misses, 3,
        "only the first pass decodes; the loop re-hits"
    );

    // On the now-warm cache a straight-line run executes the two cached `inc`s and the cached
    // backward JMP repeatedly until the batch cap fires.
    cpu.reset_perf_counters();
    assert_eq!(
        cpu.perf_counters().instructions,
        0,
        "reset zeroes the counters"
    );
    let _ = cpu.run_straight_line(&mut bus, 10_000).unwrap();
    let p = cpu.perf_counters();
    assert_eq!(p.straight_line_runs, 1, "one run");
    assert!(
        p.instructions >= 1,
        "the run retired at least the first instruction"
    );
    assert_eq!(p.brk_decode_or_branch, 0, "the cached JMP stayed in-run");
    assert_eq!(p.brk_cap, 1, "the run ended at the clock cap");
    assert_eq!(
        p.brk_step + p.brk_interrupt + p.brk_halt,
        0,
        "no other break reason fired"
    );
}

fn profile_test_cpu(code: &[u8]) -> (Cpu386, TestBus) {
    let mut memory = vec![0u8; 1024];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    (cpu, TestBus::with_memory(memory))
}

fn profile_bucket<'a>(snapshot: &'a CpuProfileSnapshot, name: &str) -> &'a CpuProfileBucket {
    snapshot
        .groups
        .iter()
        .find(|bucket| bucket.name == name)
        .expect("profile bucket exists")
}

fn profile_opcode(snapshot: &CpuProfileSnapshot, opcode: u16) -> &CpuOpcodeProfileBucket {
    snapshot
        .opcodes
        .iter()
        .find(|bucket| bucket.opcode == opcode)
        .expect("profile opcode bucket exists")
}

#[test]
fn cpu_profile_disabled_records_no_groups() {
    let (mut cpu, mut bus) = profile_test_cpu(&[0x40]); // inc ax

    cpu.cycle_no_interrupt_check(&mut bus).unwrap();

    let snapshot = cpu.profile_snapshot();
    assert!(
        snapshot.groups.iter().all(|bucket| bucket.instructions == 0
            && bucket.guest_core_clocks == 0
            && bucket.samples == 0
            && bucket.sample_wall_ns == 0),
        "profiling must be inert until explicitly enabled"
    );
    assert!(
        snapshot.opcodes.is_empty(),
        "opcode profiling must be inert until explicitly enabled"
    );
}

#[test]
fn cpu_profile_records_decode_groups() {
    let code = [
        0x05, 0x01, 0x00, // add ax,1        (alu)
        0x8b, 0xc0, // mov ax,ax       (data_move)
        0xd9, 0xe8, // fld1            (fpu)
    ];
    let (mut cpu, mut bus) = profile_test_cpu(&code);
    cpu.enable_profiling(1);

    for _ in 0..3 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    for name in ["alu", "data_move", "fpu"] {
        let bucket = profile_bucket(&snapshot, name);
        assert_eq!(bucket.instructions, 1, "{name} instruction count");
        assert_eq!(bucket.samples, 1, "{name} sampled every instruction");
    }
    for opcode in [0x05, 0x8b, 0xd9] {
        let bucket = profile_opcode(&snapshot, opcode);
        assert_eq!(bucket.instructions, 1, "opcode {opcode:#x} count");
        assert_eq!(bucket.samples, 1, "opcode {opcode:#x} samples");
    }
}

#[test]
fn cpu_profile_sample_stride_is_deterministic() {
    let (mut cpu, mut bus) = profile_test_cpu(&[0x40, 0x40, 0x40, 0x40]); // inc ax x4
    cpu.enable_profiling(2);

    for _ in 0..4 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    let bucket = profile_bucket(&snapshot, "flags_misc");
    assert_eq!(bucket.instructions, 4);
    assert_eq!(bucket.samples, 2);
    let opcode = profile_opcode(&snapshot, 0x40);
    assert_eq!(opcode.instructions, 4);
    assert_eq!(opcode.samples, 2);
}

#[test]
fn cpu_profile_opcode_counts_register_and_memory_forms() {
    let code = [
        0x8b, 0xc0, // mov ax, ax
        0x8b, 0x06, 0x20, 0x00, // mov ax, [0x0020]
    ];
    let (mut cpu, mut bus) = profile_test_cpu(&code);
    cpu.enable_profiling(1);

    for _ in 0..2 {
        cpu.cycle_no_interrupt_check(&mut bus).unwrap();
    }

    let snapshot = cpu.profile_snapshot();
    let opcode = profile_opcode(&snapshot, 0x8b);
    assert_eq!(opcode.instructions, 2);
    assert_eq!(opcode.samples, 2);
    assert_eq!(opcode.register_instructions, 1);
    assert_eq!(opcode.memory_instructions, 1);
    assert_eq!(opcode.register_samples, 1);
    assert_eq!(opcode.memory_samples, 1);
}

#[test]
fn moffs_loads_al_from_direct_offset() {
    // mov al, [0x0200] (0xa0 0x00 0x02). Byte form ignores the operand-size
    // prefix and touches only AL. It must not disturb flags.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xa0, 0x00, 0x02]);
    memory[0x200] = 0x7e;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x11ff);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // AL replaced, AH preserved, instruction is three bytes long.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x117e);
    assert_eq!(cpu.registers.eip, 3);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn moffs_stores_al_to_direct_offset() {
    // mov [0x0200], al (0xa2 0x00 0x02). Byte form writes only one byte and
    // leaves flags alone.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xa2, 0x00, 0x02]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x22a5);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0xa5);
    // The neighbouring byte is untouched by a byte store.
    assert_eq!(bus.memory[0x201], 0x00);
    assert_eq!(cpu.registers.eip, 3);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn page_translation_reads_identity_mapped_memory() {
    let mut memory = vec![0; 0x4000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2003u32.to_le_bytes());
    memory[0x2000..0x2004].copy_from_slice(&0x0000_3003u32.to_le_bytes());
    memory[0x3000] = 0x90;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 1);
}

#[test]
fn user_mode_paging_respects_the_supervisor_bit() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, writable,
    // supervisor (U/S=0) page at frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE: PT, present+rw+user
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5003u32.to_le_bytes()); // PTE[3]: frame, present+rw, U/S=0
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    let flat_cs = |rpl| SegmentRegister {
        selector: rpl,
        base: 0,
        limit: 0xffff_ffff,
        access: 0x9b,
        default_size_32: false,
    };
    let mut bus = TestBus::with_memory(memory);

    // CPL 3: a user read of the supervisor page faults with #PF, error code
    // present|user (0b101 = 0x5), and cr2 set to the faulting linear address.
    cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0003));
    cpu.cpl = 3; // this test flips CS directly, so seed the cached CPL to match
    let faulted = cpu.translate_linear(&mut bus, 0x3000, false);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x5)
            })
        ),
        "{faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // CPL 0: a 386 has no CR0.WP, so supervisor reaches the same page fine.
    cpu.registers.set_segment(SegmentIndex::Cs, flat_cs(0x0000));
    cpu.cpl = 0;
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );
}

#[test]
fn v86_paging_is_always_user_regardless_of_cs_low_bits() {
    // 386 PRM 5-24 / 15-6: a V86 task always executes at CPL 3, so paging
    // privilege (PRM ch5's U/S check) must classify every V86 access as user --
    // independent of the V86 CS selector's low two bits, which are NOT an RPL (a
    // V86 CS is a real-mode-style segment, not a descriptor selector; see
    // `current_privilege_level`'s doc comment). A monitor that maps its own
    // pages supervisor-only (U/S=0) must be unreachable from V86 even when the
    // guest's CS happens to read a multiple of 4 (RPL bits 00).
    //
    // Same page tables as `user_mode_paging_respects_the_supervisor_bit`: PD at
    // 0x1000, PT at 0x2000, linear 0x3000 -> present/writable/supervisor-only
    // frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE: PT, present+rw+user
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5003u32.to_le_bytes()); // PTE[3]: frame, present+rw, U/S=0
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    // Enter V86 with a real-mode-style CS whose low bits are 0, not 3 -- the
    // exact case a live `CS.selector & 3` formula would misclassify as
    // supervisor. `current_privilege_level` must still answer 3 here because
    // `self.cpl` is the transition-pinned cache, not a live read of CS.
    cpu.registers.eflags |= FLAG_VM;
    cpu.load_segment_real(SegmentIndex::Cs, 0xF000); // selector low bits == 0b00
    cpu.cpl = 3; // what every real V86 transition (IRET/task-switch) sets
    assert!(cpu.is_v86_mode());
    assert_eq!(
        cpu.registers.cs().selector & 3,
        0,
        "CS RPL bits are 00, not 11"
    );

    let faulted = cpu.translate_linear(&mut bus, 0x3000, false);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x5)
            })
        ),
        "a V86 access to a supervisor-only page must #PF like any other user access: {faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // Same V86 task, a user-accessible page (frame 0x4000 via PTE[2], U/S=1):
    // translation succeeds, proving the fault above was the supervisor bit and
    // not some unrelated V86 restriction.
    let mut memory = bus.memory;
    memory[0x2008..0x200c].copy_from_slice(&0x0000_4007u32.to_le_bytes());
    bus = TestBus::with_memory(memory);
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x2000, false).unwrap(),
        0x4000
    );
}

// Paged-mode fetch throughput; the case the TLB targets. Run with:
// cargo test --release -p izarravm-cpu -- --ignored --nocapture tlb_paged
#[test]
#[ignore]
fn tlb_paged_fetch_throughput() {
    let mut memory = vec![0u8; 0x10000];
    memory[0..3].copy_from_slice(&[0xfa, 0xeb, 0xfe]); // cli; jmp $
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0] -> PT
    for i in 0..16u32 {
        let off = 0x2000 + (i as usize) * 4;
        memory[off..off + 4].copy_from_slice(&((i << 12) | 0x007).to_le_bytes()); // identity PTEs
    }
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    let mut bus = TestBus::with_memory(memory);

    let iters = 50_000_000u64;
    let t = std::time::Instant::now();
    for _ in 0..iters {
        cpu.cycle(&mut bus).unwrap();
    }
    let secs = t.elapsed().as_secs_f64();
    println!(
        "tlb_paged_fetch_throughput: {iters} paged instructions in {secs:.3}s = {:.1} M instr/s",
        iters as f64 / secs / 1.0e6
    );
}

#[test]
fn tlb_caches_translations_and_is_non_snooping_until_flushed() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 -> present+rw+user frame 0x5000.
    let mut memory = vec![0; 0x7000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes()); // PDE[0]
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5007u32.to_le_bytes()); // PTE[3]
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PG;
    cpu.control.cr3 = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    // First translation walks the table and fills the TLB.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // Repoint the PTE to frame 0x6000 in memory with no INVLPG / CR3 reload.
    bus.memory[0x200c..0x2010].copy_from_slice(&0x0000_6007u32.to_le_bytes());

    // Real x86 TLBs do not snoop page-table writes: the stale cached frame is
    // returned until an explicit flush -- the faithful behavior a guest relies
    // on (it must INVLPG / reload CR3 after editing a PTE).
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // After a flush the next access re-walks and sees the new mapping.
    cpu.tlb.flush();
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x6000
    );
}

#[test]
fn cr0_wp_gates_supervisor_writes_to_read_only_pages() {
    // PD at 0x1000, PT at 0x2000. Linear 0x3000 maps to a present, read-only
    // (R/W=0), supervisor (U/S=0) page at frame 0x5000.
    let mut memory = vec![0; 0x6000];
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2001u32.to_le_bytes()); // PDE: PT, present, R/W=0, U/S=0
    memory[0x200c..0x2010].copy_from_slice(&0x0000_5001u32.to_le_bytes()); // PTE[3]: frame, present, R/W=0, U/S=0
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    // Supervisor: CPL 0.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0000,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    let mut bus = TestBus::with_memory(memory);

    // WP clear (the 386 default): a supervisor write to the read-only page
    // succeeds and resolves to the mapped frame.
    assert_eq!(cpu.control.cr0 & CR0_WP, 0);
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, true).unwrap(),
        0x5000
    );

    // A supervisor read always passes regardless of WP.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );

    // WP set (the 486 feature): the same supervisor write now faults #PF with
    // error code present|write (bits 0 and 1 -> 0b011 = 0x3); the U/S bit is 0
    // because the access is supervisor, and cr2 holds the faulting address.
    cpu.control.cr0 |= CR0_WP;
    let faulted = cpu.translate_linear(&mut bus, 0x3000, true);
    assert!(
        matches!(
            faulted,
            Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(0x3)
            })
        ),
        "{faulted:?}"
    );
    assert_eq!(cpu.control.cr2, 0x3000);

    // A supervisor read is unaffected by WP and still resolves.
    assert_eq!(
        cpu.translate_linear(&mut bus, 0x3000, false).unwrap(),
        0x5000
    );
}

#[test]
fn stosb_writes_al_to_es_di() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xaa;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_edi(0x200);
    cpu.write_gpr8(0, b'S');
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], b'S');
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn rep_stosb_fills_es_di() {
    // rep stosb (0xf3 0xaa), cx=3, al=0xee. Fills 3 bytes at es:di, cx -> 0, di += 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xaa]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.write_gpr8(0, 0xee);
    cpu.registers.set_edi(0x300);
    cpu.registers.set_ecx(3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x300..0x303], &[0xee, 0xee, 0xee]);
    assert_eq!(cpu.registers.edi(), 0x303);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn lodsw_loads_ax_and_advances_si() {
    // lodsw (0xad). [ds:si]=0x1234 (LE) -> ax; si += 2.
    let mut memory = vec![0; 1024];
    memory[0] = 0xad;
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.registers.esi(), 0x102);
}

#[test]
fn out_dx_al_uses_dx_port() {
    let mut memory = vec![0; 16];
    memory[0..6].copy_from_slice(&[0xba, 0xf8, 0x03, 0xb0, b'X', 0xee]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();

    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|cycle| { cycle.kind == BusAccessKind::IoWrite && cycle.address == 0x03f8 })
    );
}

#[test]
fn test_byte_sets_sign_flag() {
    // test al, al with al = 0x80  (0x84 modrm 0xc0). SF must reflect bit 7.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x84, 0xc0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn test_word_immediate_group_f7() {
    // test bx, 0x0001  (0xf7 /0, modrm 0xc3, imm 0x0001). bx=0x0002 -> ZF set.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0xf7, 0xc3, 0x01, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn group81_add_memory_with_displacement_and_immediate() {
    // add word [bx+0x10], 0x0102  (0x81 /0, modrm 0x47, disp 0x10, imm 0x0102)
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0x81, 0x47, 0x10, 0x02, 0x01, 0xf4]);
    memory[0x210..0x212].copy_from_slice(&0x0003u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x210], bus.memory[0x211]]),
        0x0105
    );
    assert_eq!(cpu.registers.eip, 5); // opcode + modrm + disp8 + imm16
}

#[test]
fn group83_sign_extends_immediate() {
    // sub bx, -1  (0x83 /5, modrm 0xeb, imm 0xff -> -1)
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x83, 0xeb, 0xff]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0006); // 5 - (-1) = 6
}

#[test]
fn add_rm_reg_byte_writes_memory_with_displacement() {
    // add [bx+0x10], al   (opcode 0x00, modrm 0x47, disp 0x10)
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x00, 0x47, 0x10, 0xf4]);
    memory[0x210] = 0x01; // [bx+0x10] initial
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    cpu.write_gpr8(0, 0x05); // al
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x210], 0x06);
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no double-fetch
}

#[test]
fn sub_reg_rm_sets_flags() {
    // sub al, bl  (opcode 0x2a, modrm 0xc3)
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x2a, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x05); // al
    cpu.write_gpr8(3, 0x05); // bl
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(0), 0x00);
    assert!(cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn cmp_does_not_write_back() {
    // cmp al, 0x10 is form via 0x3c (AL, imm8)
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x3c, 0x10]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x10);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(0), 0x10); // unchanged
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn alu_add_byte_sets_carry_zero_and_aux() {
    let mut cpu = Cpu386::default();
    let result = cpu.alu(0, 0xff, 0x01, BusWidth::Byte);
    assert_eq!(result, 0x00);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn alu_adc_uses_carry_in() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.alu(2, 0x01, 0x01, BusWidth::Word); // ADC 1,1 with CF=1 -> 3
    assert_eq!(result, 0x0003);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn alu_sub_byte_sets_borrow_and_sign() {
    let mut cpu = Cpu386::default();
    let result = cpu.alu(5, 0x00, 0x01, BusWidth::Byte); // 0 - 1 = 0xff
    assert_eq!(result, 0xff);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn alu_sbb_uses_borrow_in() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.alu(3, 0x05, 0x02, BusWidth::Word); // 5 - 2 - 1 = 2
    assert_eq!(result, 0x0002);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn alu_logic_clears_carry_overflow_leaves_aux() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_AF, true);
    let result = cpu.alu(4, 0xf0, 0x0f, BusWidth::Byte); // AND -> 0
    assert_eq!(result, 0x00);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF)); // AND leaves AF untouched (undefined)
}

#[test]
fn alu_add_byte_overflow_without_carry() {
    let mut cpu = Cpu386::default();
    let result = cpu.alu(0, 0x7f, 0x01, BusWidth::Byte); // 127 + 1 -> 0x80
    assert_eq!(result, 0x80);
    assert!(cpu.flag(FLAG_OF)); // signed overflow, isolated from carry
    assert!(!cpu.flag(FLAG_CF)); // no unsigned carry
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn alu_sbb_borrow_in_with_max_subtrahend() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_CF, true); // borrow in
    let result = cpu.alu(3, 0x00, 0xff, BusWidth::Byte); // 0 - 0xff - 1
    assert_eq!(result, 0x00);
    assert!(cpu.flag(FLAG_CF)); // b + borrow must not wrap to 0 and clear CF
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn alu_parity_uses_low_byte_only() {
    let mut cpu = Cpu386::default();
    let result = cpu.alu(0, 0x00ff, 0x0001, BusWidth::Word); // -> 0x0100
    assert_eq!(result, 0x0100);
    assert!(cpu.flag(FLAG_PF)); // low byte 0x00 is even parity; full word would be odd
}

#[test]
fn alu_sign_flag_word_uses_bit15() {
    let mut cpu = Cpu386::default();
    let result = cpu.alu(0, 0x8000, 0x0000, BusWidth::Word);
    assert_eq!(result, 0x8000);
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn inc_reg_preserves_carry_flag() {
    // inc ax (0x40) with CF set: AX increments, CF stays set, AF set by 0xff+1.
    let mut memory = vec![0; 16];
    memory[0] = 0x40;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x00ff);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF)); // INC must not touch CF
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn dec_reg_sets_zero_and_keeps_carry_clear() {
    // dec ax (0x48) with CF clear: AX -> 0, ZF set, CF still clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x48;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn inc_word_memory_via_ff_group() {
    // inc word [bx]  (0xff /0, modrm 0x07). 0x00ff -> 0x0100.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0x07]);
    memory[0x200..0x202].copy_from_slice(&0x00ffu16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x0100
    );
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn call_near_indirect_register_pushes_return_and_jumps() {
    // call ax  (0xff /2, modrm 0xd0). Pushes return eip (2), jumps to ax.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xd0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Ax, 0x0050);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0050);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0002
    );
}

#[test]
fn jmp_near_indirect_sets_eip_without_push() {
    // jmp bx  (0xff /4, modrm 0xe3).
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xff, 0xe3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Bx, 0x0030);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0030);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0100); // no push
}

#[test]
fn push_rm_writes_value_and_decrements_sp() {
    // push cx  (0xff /6, modrm 0xf1).
    let mut memory = vec![0; 256];
    memory[0..2].copy_from_slice(&[0xff, 0xf1]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Cx, 0xbeef);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xbeef
    );
}

#[test]
fn inc_byte_memory_with_displacement() {
    // inc byte [bx+0x10]  (0xfe /0, modrm 0x47, disp 0x10). 0x7f -> 0x80.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xfe, 0x47, 0x10]);
    memory[0x210] = 0x7f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x210], 0x80);
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_OF)); // 0x7f + 1 byte overflow
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8
}

#[test]
fn inc_word_overflow_sets_of_and_sf() {
    // inc ax (0x40) on 0x7fff: -> 0x8000, OF and SF set, CF preserved.
    let mut memory = vec![0; 16];
    memory[0] = 0x40;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x7fff);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_CF)); // preserved
}

#[test]
fn cmp_memory_form_issues_no_write() {
    // cmp [bx], al  (0x38 modrm 0x07). Equal operands -> ZF, and no write cycle.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x38, 0x07, 0xf4]);
    memory[0x200] = 0x42;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x200);
    cpu.write_gpr8(0, 0x42); // al
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(bus.memory[0x200], 0x42); // unchanged
    assert!(
        !bus.trace
            .cycles()
            .iter()
            .any(|cycle| cycle.kind == BusAccessKind::DataWrite)
    );
}

#[test]
fn incdec_preserve_carry_both_directions() {
    // DEC with CF set leaves CF set.
    let mut memory = vec![0; 16];
    memory[0] = 0x48; // dec ax
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0004);
    assert!(cpu.flag(FLAG_CF));

    // INC with CF clear leaves CF clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x40; // inc ax
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0006);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn dec_word_overflow_sets_of() {
    // dec ax (0x48) on 0x8000 -> 0x7fff: OF set, SF clear.
    let mut memory = vec![0; 16];
    memory[0] = 0x48;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x7fff);
    assert!(cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn call_near_indirect_memory_displacement_return_addr() {
    // call [bx+0x10] (0xff /2, modrm 0x57, disp 0x10): 3-byte instruction,
    // return address must be computed after the displacement fetch.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xff, 0x57, 0x10]);
    memory[0x210..0x212].copy_from_slice(&0x0080u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.write_reg16(Reg16::Bx, 0x200);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0080);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0003
    );
}

#[test]
fn push_sp_uses_pre_decrement_value() {
    // push sp (0xff /6, modrm 0xf4): the 386 pushes SP before the decrement.
    let mut memory = vec![0; 256];
    memory[0..2].copy_from_slice(&[0xff, 0xf4]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0100
    );
}

#[test]
fn inc_dword_uses_32bit_width() {
    // 0x66 0x40 = inc eax (32-bit operand): 0x0000ffff -> 0x00010000.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0x66, 0x40]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_ffff);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x0001_0000);
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn shl_word_by_one_sets_of_and_clears_cf() {
    // shl ax,1 (0xd1 /4, modrm 0xe0). 0x4000 -> 0x8000, CF=0 (old bit15), OF=1, SF=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xe0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shr_word_by_one_sets_cf_and_of() {
    // shr ax,1 (0xd1 /5, modrm 0xe8). 0x8001 -> 0x4000, CF=1, OF=msb(orig)=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xe8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x4000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF)); // result 0x4000 is positive
}

#[test]
fn shl_dword_by_one_via_operand_size_prefix() {
    // shl eax,1 (0x66 0xd1 /4, modrm 0xe0). 0x4000_0000 -> 0x8000_0000, CF=0, OF=1, SF=1.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xd1, 0xe0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x4000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert_eq!(cpu.registers.eip, 3); // prefix + opcode + modrm
}

#[test]
fn repeated_operand_size_prefix_stays_active() {
    // 66 66 d1 e0 = shl eax,1 with a redundant operand-size prefix. The
    // second 66 must not cancel the first, so this stays a 32-bit shift.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x66, 0xd1, 0xe0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x4000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0000);
    assert_eq!(cpu.registers.eip, 4); // two prefixes + opcode + modrm
}

#[test]
fn sar_word_by_one_preserves_sign_and_clears_of() {
    // sar ax,1 (0xd1 /7, modrm 0xf8). 0x8001 -> 0xc000, CF=1, OF=0, SF=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xf8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xc000);
    assert!(cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shl_byte_via_c0_imm_only_touches_low_byte() {
    // shl al,1 (0xc0 /4, modrm 0xe0, imm 0x01). ax=0xff81 -> al 0x81<<1=0x02, ah preserved.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xc0, 0xe0, 0x01]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xff81);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff02);
    assert!(cpu.flag(FLAG_CF)); // old bit7 of 0x81
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + imm8
}

#[test]
fn shl_word_by_imm_count() {
    // shl ax,4 (0xc1 /4, modrm 0xe0, imm 0x04). 0x0001 -> 0x0010.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xc1, 0xe0, 0x04]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0010);
    assert_eq!(cpu.registers.eip, 3);
}

#[test]
fn shift_count_masked_to_five_bits() {
    // shl ax,cl with cl=33 (0xd3 /4, modrm 0xe0). 33 & 0x1f == 1, so one shift.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    cpu.write_reg16(Reg16::Cx, 33);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
}

#[test]
fn shift_count_zero_touches_no_flags() {
    // shl ax,cl with cl=32 (0xd3 /4). 32 & 0x1f == 0: operand and flags unchanged.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd3, 0xe0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Cx, 32);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert!(cpu.flag(FLAG_CF)); // unchanged: a zero count touches no flags
}

#[test]
fn rol_word_by_one() {
    // rol ax,1 (0xd1 /0, modrm 0xc0). 0x8000 -> 0x0001, CF=1, OF=msb^cf=0^1=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn ror_word_by_one() {
    // ror ax,1 (0xd1 /1, modrm 0xc8). 0x0001 -> 0x8000, CF=1, OF=msb^next=1^0=1.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn rcl_word_rotates_through_carry() {
    // rcl ax,1 (0xd1 /2, modrm 0xd0). ax=0x0000, CF=1 -> 0x0001, CF=0 (old msb=0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xd0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001); // carry rotated into bit 0
    assert!(!cpu.flag(FLAG_CF)); // old msb (0) rotated out
    assert!(!cpu.flag(FLAG_OF)); // result_msb(0) ^ cf(0)
}

#[test]
fn rcr_word_rotates_through_carry() {
    // rcr ax,1 (0xd1 /3, modrm 0xd8). ax=0x0000, CF=1 -> 0x8000, CF=0 (old bit0=0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xd8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000); // carry rotated into bit 15
    assert!(!cpu.flag(FLAG_CF)); // old bit0 (0) rotated out
    assert!(cpu.flag(FLAG_OF)); // result_msb(1) ^ result_bit14(0)
}

#[test]
fn rotate_leaves_sign_zero_parity_untouched() {
    // rol ax,1: rotates touch only CF/OF, never SF/ZF/PF. Set ZF first, then
    // rotate to a nonzero result and confirm ZF survives.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd1, 0xc0]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0001);
    assert!(cpu.flag(FLAG_ZF)); // unchanged by a rotate
}

#[test]
fn ror_byte_by_cl_multi_bit() {
    // ror al,cl with cl=3 (0xd2 /1, modrm 0xc8). Exercises the byte width
    // (msb 0x80, shift by bits-1=7) and a multi-bit count. al 0x01 ror 3 = 0x20.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xd2, 0xc8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Cx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0020); // ah preserved, al rotated
    assert!(!cpu.flag(FLAG_CF)); // last bit out is 0
}

#[test]
fn not_byte_leaves_flags_untouched() {
    // not bl (0xf6 /2, modrm 0xd3). 0x0f -> 0xf0; NOT affects no flags.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xd3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x000f);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xf0);
    assert!(cpu.flag(FLAG_CF)); // unchanged
    assert!(cpu.flag(FLAG_ZF)); // unchanged
}

#[test]
fn neg_byte_sets_carry_and_sign() {
    // neg bl (0xf6 /3, modrm 0xdb). 0x01 -> 0xff; CF set, SF set, ZF clear.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0001);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0xff);
    assert!(cpu.flag(FLAG_CF)); // operand nonzero
    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn neg_zero_clears_carry_and_sets_zero() {
    // neg bl of 0x00 -> 0x00; CF clear, ZF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x00);
    assert!(!cpu.flag(FLAG_CF)); // operand zero
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn neg_byte_overflow_at_0x80() {
    // neg bl of 0x80 -> 0x80; OF set (only value that negates to itself), CF and SF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xdb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0080);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x80);
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn not_word_via_f7_complements() {
    // not bx (0xf7 /2, modrm 0xd3). 0x0ff0 -> 0xf00f; flags unchanged.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xd3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0ff0);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xf00f);
    assert!(cpu.flag(FLAG_CF)); // NOT touches no flags
}

#[test]
fn mul_byte_sets_carry_when_high_nonzero() {
    // mul bl (0xf6 /4, modrm 0xe3). al=0x10, bl=0x10 -> ax=0x0100; CF/OF set (ah != 0).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn mul_byte_clears_carry_when_high_zero() {
    // mul bl. al=0x05, bl=0x03 -> ax=0x000f; CF/OF clear.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xe3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x000f);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn mul_word_writes_dx_ax_preserving_high_halves() {
    // mul bx (0xf7 /4, modrm 0xe3). ax=0x1000, bx=0x0010 -> product 0x0010_0000:
    // ax=0x0000, dx=0x0001; CF/OF set. High 16 bits of EAX/EDX must survive.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xe3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xaaaa_1000);
    cpu.registers.set_edx(0xbbbb_0000);
    cpu.registers.set_ebx(0x0000_0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xaaaa_0000); // ax=0, high preserved
    assert_eq!(cpu.registers.edx(), 0xbbbb_0001); // dx=1, high preserved
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_byte_clears_carry_when_result_fits() {
    // imul bl (0xf6 /5, modrm 0xeb). al=0xff(-1), bl=0x02(+2) -> ax=0xfffe(-2);
    // CF/OF clear because the high half is the sign extension of the low half.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x00ff);
    cpu.write_reg16(Reg16::Bx, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_byte_sets_carry_when_result_overflows() {
    // imul bl. al=0x10(+16), bl=0x10(+16) -> ax=0x0100(+256); the low byte is 0x00,
    // its sign extension is 0x0000 != 0x0100, so CF/OF set.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xeb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0100);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn mul_dword_writes_edx_eax() {
    // mul ebx (0x66 0xf7 /4, modrm 0xe3). eax=0x0001_0000 * ebx=0x0001_0000
    // = 0x1_0000_0000 -> eax=0, edx=1; CF/OF set. Exercises the u64 dword path.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xe3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0001_0000);
    cpu.registers.set_ebx(0x0001_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_0000);
    assert_eq!(cpu.registers.edx(), 0x0000_0001);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn div_byte_writes_quotient_and_remainder() {
    // div bl (0xf6 /6, modrm 0xf3). ax=0x0011(17), bl=0x05 -> al=3, ah=2.
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xf3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0011);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0203); // ah=2 (rem), al=3 (quot)
}

#[test]
fn div_word_writes_ax_and_dx() {
    // div bx (0xf7 /6, modrm 0xf3). dx:ax = 0x0000:0x0011 (17), bx=5 -> ax=3 (quot), dx=2 (rem).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf7, 0xf3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Dx, 0x0000);
    cpu.write_reg16(Reg16::Ax, 0x0011);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0003);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0002);
}

#[test]
fn idiv_byte_negative_dividend_truncates_toward_zero() {
    // idiv bl (0xf6 /7, modrm 0xfb). ax=-17=0xffef, bl=+5 -> quot=-3 (0xfd), rem=-2 (0xfe).
    let mut memory = vec![0; 16];
    memory[0..2].copy_from_slice(&[0xf6, 0xfb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffef);
    cpu.write_reg16(Reg16::Bx, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xfd); // al = -3
    assert_eq!((cpu.read_reg16(Reg16::Ax) >> 8) & 0xff, 0xfe); // ah = -2
}

/// Give a real-mode `TestBus` a full 1 MiB image (room for a wrap-safe stack and every
/// vector's IVT slot) and point vector 0 (#DE) at a distinguishing trap address, then run
/// one `cycle` and assert the CPU landed there -- i.e. the fault was DELIVERED through
/// `real_mode_interrupt`, not raised as a host-fatal error. Batch A converted #DE from
/// `CpuError::DivideError` (host-fatal) to `InternalFault::Exception { vector: 0, .. }`
/// (guest-deliverable), so a real-mode DIV-by-zero now runs to completion (`cycle` returns
/// `Ok`) with CS:IP retargeted at the IVT's vector-0 entry instead of erroring `cycle` itself.
const DE_TRAP_CS: u16 = 0x0200;
const DE_TRAP_IP: u16 = 0x0010;
// Code origin for the de_trap_bus helpers: away from offset 0, which is the vector-0 IVT
// slot these buses populate (code and the IVT slot must not overlap).
const DE_CODE_ORIGIN: u32 = 0x20;

fn expect_de_delivered<B: CpuBus>(cpu: &mut Cpu386, bus: &mut B) {
    let outcome = cpu
        .cycle(bus)
        .expect("a delivered #DE must not error `cycle`");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, DE_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(DE_TRAP_IP));
}

fn de_trap_bus(code: &[u8]) -> TestBus {
    let mut memory = vec![0u8; 0x1_0000];
    let origin = DE_CODE_ORIGIN as usize;
    memory[origin..origin + code.len()].copy_from_slice(code);
    // IVT[0] (bytes 0..4): IP then CS, little-endian.
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    TestBus::with_memory(memory)
}

#[test]
fn div_by_zero_returns_error_without_writes() {
    // div bl with bl=0 -> #DE delivered through the real-mode IVT; ax unchanged.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let mut bus = de_trap_bus(&[0xf6, 0xf3]);

    expect_de_delivered(&mut cpu, &mut bus);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234); // no writes
}

#[test]
fn div_quotient_overflow_returns_error() {
    // div bl: ax=0xffff, bl=0x01 -> quotient 0xffff > 0xff -> #DE delivered.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x0001);
    let mut bus = de_trap_bus(&[0xf6, 0xf3]);

    expect_de_delivered(&mut cpu, &mut bus);
}

#[test]
fn div_dword_writes_eax_edx() {
    // div ebx (0x66 0xf7 /6, modrm 0xf3). edx:eax = 0x1_0000_0005, ebx=2
    // -> quot=0x8000_0002, rem=1. Exercises the u64 dword path.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x66, 0xf7, 0xf3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_edx(0x0000_0001);
    cpu.registers.set_eax(0x0000_0005);
    cpu.registers.set_ebx(0x0000_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x8000_0002); // quotient
    assert_eq!(cpu.registers.edx(), 0x0000_0001); // remainder
}

#[test]
fn idiv_dword_min_over_negative_one_is_divide_error() {
    // idiv ebx (0x66 0xf7 /7, modrm 0xfb). edx:eax = i64::MIN, ebx = -1.
    // checked_div catches the overflow so this is #DE (delivered), not a panic.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = DE_CODE_ORIGIN;
    cpu.registers.set_esp(0x2000);
    cpu.registers.set_edx(0x8000_0000);
    cpu.registers.set_eax(0x0000_0000);
    cpu.registers.set_ebx(0xffff_ffff);
    let mut bus = de_trap_bus(&[0x66, 0xf7, 0xfb]);

    expect_de_delivered(&mut cpu, &mut bus);
}

#[test]
fn movsb_copies_and_increments_when_df_clear() {
    // movsb (0xa4). [ds:si]=0x42 -> [es:di]; si and di increment (DF=0).
    let mut memory = vec![0; 1024];
    memory[0] = 0xa4;
    memory[0x100] = 0x42;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0x42);
    assert_eq!(cpu.registers.esi(), 0x101);
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn movsb_decrements_when_df_set() {
    // movsb with DF=1: si and di decrement.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa4;
    memory[0x100] = 0x42;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, true);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0x42);
    assert_eq!(cpu.registers.esi(), 0x0ff);
    assert_eq!(cpu.registers.edi(), 0x1ff);
}

#[test]
fn rep_movsb_copies_cx_bytes() {
    // rep movsb (0xf3 0xa4) with cx=3 copies 3 bytes, leaves cx=0, advances si/di by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x103].copy_from_slice(&[1, 2, 3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x200..0x203], &[1, 2, 3]);
    assert_eq!(cpu.registers.esi(), 0x103);
    assert_eq!(cpu.registers.edi(), 0x203);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_iterations, 3);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);
}

#[test]
fn rep_movsb_df_set_uses_correct_slow_path() {
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100..0x104].copy_from_slice(&[1, 2, 3, 4]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, true);
    cpu.registers.set_esi(0x103);
    cpu.registers.set_edi(0x203);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(&bus.memory[0x200..0x204], &[1, 2, 3, 4]);
    assert_eq!(cpu.registers.esi(), 0x0ff);
    assert_eq!(cpu.registers.edi(), 0x1ff);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_iterations, 4);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 0);
}

#[test]
fn rep_movsb_with_zero_count_does_nothing() {
    // rep movsb with cx=0 performs no access and leaves si/di/cx unchanged.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa4]);
    memory[0x100] = 0x42;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(0);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x200], 0); // no write
    assert_eq!(cpu.registers.esi(), 0x100);
    assert_eq!(cpu.registers.edi(), 0x200);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn cmpsb_equal_sets_zero_flag() {
    // cmpsb (0xa6). [ds:si]=0x55, [es:di]=0x55 -> equal, ZF set.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa6;
    memory[0x100] = 0x55;
    memory[0x200] = 0x55;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.esi(), 0x101);
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn cmpsb_unequal_clears_zero_flag() {
    // cmpsb. [ds:si]=0x10, [es:di]=0x20 -> 0x10-0x20 borrows: ZF clear, CF set.
    let mut memory = vec![0; 1024];
    memory[0] = 0xa6;
    memory[0x100] = 0x10;
    memory[0x200] = 0x20;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_CF)); // 0x10 < 0x20
    assert_eq!(cpu.registers.esi(), 0x101); // si advances even when unequal
    assert_eq!(cpu.registers.edi(), 0x201);
}

#[test]
fn scasb_compares_al_with_es_di() {
    // scasb (0xae). al=0x41, [es:di]=0x41 -> ZF set; di increments, si untouched.
    let mut memory = vec![0; 1024];
    memory[0] = 0xae;
    memory[0x200] = 0x41;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x41);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.edi(), 0x201);
    assert_eq!(cpu.registers.esi(), 0x100); // SCAS does not touch SI
}

#[test]
fn rep_fast_paths_cover_stos_lods_cmps_and_scas() {
    let memory = vec![0; 2048];
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.write_gpr8(0, 0x7e);
    cpu.registers.set_edi(0x300);
    cpu.registers.set_ecx(4);
    cpu.run_string(
        &mut bus,
        StringOp::Stos,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(&bus.memory[0x300..0x304], &[0x7e; 4]);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 4);

    cpu.reset_perf_counters();
    bus.memory[0x400..0x403].copy_from_slice(&[1, 2, 3]);
    cpu.registers.set_esi(0x400);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        StringOp::Lods,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert_eq!(cpu.read_gpr8(0), 3);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);

    cpu.reset_perf_counters();
    bus.memory[0x500..0x503].copy_from_slice(&[1, 2, 9]);
    bus.memory[0x600..0x603].copy_from_slice(&[1, 2, 3]);
    cpu.registers.set_esi(0x500);
    cpu.registers.set_edi(0x600);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        StringOp::Cmps,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repe),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 3);

    cpu.reset_perf_counters();
    bus.memory[0x700..0x703].copy_from_slice(&[1, 2, 3]);
    cpu.write_gpr8(0, 2);
    cpu.registers.set_edi(0x700);
    cpu.registers.set_ecx(3);
    cpu.run_string(
        &mut bus,
        StringOp::Scas,
        BusWidth::Byte,
        Prefixes {
            rep: Some(RepKind::Repne),
            ..Default::default()
        },
        AddressSize::Word,
    )
    .unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.ecx(), 1);
    assert_eq!(cpu.perf_counters().rep_string_fast_iterations, 2);
}

#[test]
fn repe_cmpsb_stops_on_first_mismatch() {
    // repe cmpsb (0xf3 0xa6), cx=4. Source "AABB" vs dest "AACC": the third byte
    // (index 2) is the B/C mismatch, so the repeat stops there with ZF clear after
    // 3 iterations; cx counts 4 -> 3 -> 2 -> 1, si/di advance by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf3, 0xa6]);
    memory[0x100..0x104].copy_from_slice(b"AABB");
    memory[0x200..0x204].copy_from_slice(b"AACC");
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x100);
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF)); // stopped on the index-2 mismatch (B != C)
    assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, then ZF clear stops
    assert_eq!(cpu.registers.esi(), 0x103);
    assert_eq!(cpu.registers.edi(), 0x203);
}

#[test]
fn repne_scasb_stops_on_match() {
    // repne scasb (0xf2 0xae), cx=4, al='C'. Dest "AACA": scans until the match at
    // index 2, stopping with ZF set after 3 iterations; cx counts 4 -> 3 -> 2 -> 1,
    // di advances by 3.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xf2, 0xae]);
    memory[0x200..0x204].copy_from_slice(b"AACA");
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.write_gpr8(0, b'C');
    cpu.registers.set_edi(0x200);
    cpu.registers.set_ecx(4);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF)); // matched 'C' at index 2
    assert_eq!(cpu.registers.ecx(), 1); // 4 -> 3 -> 2 -> 1, match stops
    assert_eq!(cpu.registers.edi(), 0x203);
}

#[test]
fn movsb_honors_source_segment_override() {
    // es: movsb (0x26 0xa4). With ds=0 and es base 0x200, the override reads the
    // source from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
    let mut memory = vec![0; 0x400];
    memory[0..2].copy_from_slice(&[0x26, 0xa4]);
    memory[0x210] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_DF, false);
    cpu.registers.set_esi(0x10);
    cpu.registers.set_edi(0x30);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x230], 0x99); // es:di destination
    assert_eq!(bus.memory[0x10], 0); // ds:si source was not used
}

#[test]
fn lea_loads_effective_address() {
    // lea bx, [si+0x10]  (0x8d 0x5c 0x10). bx <- si + 0x10, no memory access:
    // the byte at the computed address must not be loaded.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x8d, 0x5c, 0x10]);
    memory[0x110] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esi(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0110);
}

#[test]
fn lea_with_register_operand_delivers_ud() {
    // lea ax, ax  (0x8d 0xc0, mod=3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x8d, 0xc0]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lds_loads_offset_and_ds() {
    // lds bx, [0x0200]  (0xc5 0x1e 0x00 0x02). Loads the far pointer at DS:0x0200:
    // BX <- word[0x0200], DS <- word[0x0202]. No flags change.
    let mut memory = vec![0; 0x1000];
    memory[0..4].copy_from_slice(&[0xc5, 0x1e, 0x00, 0x02]);
    memory[0x0200] = 0x34; // offset low
    memory[0x0201] = 0x12; // offset high -> 0x1234
    memory[0x0202] = 0x00; // selector low
    memory[0x0203] = 0x90; // selector high -> 0x9000
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x9000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x9000 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn les_loads_offset_and_es() {
    // les di, [bx]  (0xc4 0x3f). With BX=0x0300 it loads DS:0x0300:
    // DI <- word[0x0300], ES <- word[0x0302]. No flags change.
    let mut memory = vec![0; 0x1000];
    memory[0..2].copy_from_slice(&[0xc4, 0x3f]);
    memory[0x0300] = 0x78; // offset low
    memory[0x0301] = 0x56; // offset high -> 0x5678
    memory[0x0302] = 0x00; // selector low
    memory[0x0303] = 0xb8; // selector high -> 0xb800
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_ebx(0x0300);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Di), 0x5678);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).selector, 0xb800);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).base, 0xb800 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn lds_with_register_operand_delivers_ud() {
    // lds ax, bx  (0xc5 0xc3, mod=3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xc5, 0xc3]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lss_real_mode_16bit_loads_offset_and_ss_and_arms_shadow() {
    // lss bx, [0x200]  (0F B2 1E 00 02). Loads the far pointer at DS:0x200:
    // BX <- word[0x200], SS <- word[0x202]. No flags change, but LSS arms the
    // one-instruction interrupt shadow (386 PRM 11-16), exactly like MOV SS/POP SS.
    // No interrupt is pending going in (IF true with nothing pending is the ordinary
    // case); the deferred-delivery behavior itself is `lss_interrupt_shadow_defers_
    // a_pending_irq_by_one_instruction` below.
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb2, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34; // offset low
    memory[0x201] = 0x12; // offset high -> 0x1234
    memory[0x202] = 0x00; // selector low
    memory[0x203] = 0x90; // selector high -> 0x9000
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, true);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x9000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).base, 0x9000 << 4);
    assert_eq!(cpu.registers.eflags, flags_before);
    assert!(
        cpu.interrupt_shadow,
        "LSS must arm the one-instruction interrupt shadow"
    );
}

#[test]
fn lss_interrupt_shadow_defers_a_pending_irq_by_one_instruction() {
    // Same shape as `sti_interrupt_shadow_defers_interrupt_by_one_instruction`, but the
    // shadow is armed by LSS instead of STI: a pending IRQ must not be taken until the
    // instruction AFTER LSS has run.
    let mut memory = vec![0u8; 0x300];
    memory[0..5].copy_from_slice(&[0x0f, 0xb2, 0x1e, 0x00, 0x02]); // lss bx, [0x200]
    memory[5] = 0x90; // NOP -- executes before the interrupt is taken (shadow)
    memory[6] = 0x90; // NOP -- not reached; interrupt taken instead
    memory[0x200] = 0x00; // offset -> 0x0000
    memory[0x201] = 0x00;
    memory[0x202] = 0x00; // selector -> 0x0000 (SS stays flat at base 0 in real mode)
    memory[0x203] = 0x00;
    // IVT entry for vector 0x08 (IRQ0) at byte offset 0x20: offset=0x0208, segment=0.
    memory[0x20..0x22].copy_from_slice(&0x0208u16.to_le_bytes());
    memory[0x22..0x24].copy_from_slice(&0x0000u16.to_le_bytes());
    memory[0x208] = 0xcf; // IRET at the handler target (not reached in this test)

    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    cpu.set_flag(FLAG_IF, true);

    let mut bus = TestBus::with_memory(memory);
    // No interrupt pending yet: it "arrives" during LSS's execution window, which is
    // exactly the case the shadow exists to cover -- an IRQ landing between the LSS
    // and the next instruction boundary must wait for that boundary.

    // Cycle 1: LSS. SS reloads; the shadow arms. eip advances normally.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 5,
        "eip must be 5 after LSS -- NOP not yet executed"
    );
    assert!(
        cpu.interrupt_shadow,
        "the shadow must be armed immediately after LSS runs"
    );
    // The IRQ arrives now, after LSS has already committed.
    bus.pending_irq = Some(8);

    // Cycle 2: NOP. Shadow consumed at cycle start -> interrupt check skipped -> NOP
    // executes -> eip advances to 6. IRQ still pending.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 6,
        "eip must be 6 after NOP -- shadow let NOP through"
    );
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must still be pending after NOP (shadow consumed, interrupt check skipped)"
    );

    // Cycle 3: no shadow, IF set, IRQ pending -> interrupt is acknowledged before fetch.
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.pending_irq.is_none(),
        "interrupt must be taken after the shadow expires"
    );
}

#[test]
fn lss_32bit_operand_size_loads_esp_wide_offset() {
    // 66 0F B2 1E 00 02 -- lss ebx, [0x200] (operand-size override to 32-bit).
    // EBX <- dword[0x200], SS <- word[0x204].
    let mut memory = vec![0u8; 0x1000];
    memory[0..6].copy_from_slice(&[0x66, 0x0f, 0xb2, 0x1e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    memory[0x204..0x206].copy_from_slice(&0x9000u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ebx(), 0x1122_3344);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x9000);
}

#[test]
fn lfs_loads_offset_and_fs() {
    // lfs bx, [0x200]  (0F B4 1E 00 02). No interrupt shadow -- only LSS arms it.
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb4, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34;
    memory[0x201] = 0x12;
    memory[0x202] = 0x00;
    memory[0x203] = 0x70;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    cpu.set_flag(FLAG_IF, true);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0x7000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).base, 0x7000 << 4);
    assert!(
        !cpu.interrupt_shadow,
        "LFS must not arm the SS interrupt shadow"
    );
}

#[test]
fn lgs_loads_offset_and_gs() {
    // lgs bx, [0x200]  (0F B5 1E 00 02).
    let mut memory = vec![0u8; 0x1000];
    memory[0..5].copy_from_slice(&[0x0f, 0xb5, 0x1e, 0x00, 0x02]);
    memory[0x200] = 0x34;
    memory[0x201] = 0x12;
    memory[0x202] = 0x00;
    memory[0x203] = 0x60;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0x6000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).base, 0x6000 << 4);
    assert!(
        !cpu.interrupt_shadow,
        "LGS must not arm the SS interrupt shadow"
    );
}

#[test]
fn lss_with_register_operand_delivers_ud() {
    // lss bx, ax encoded with mod=3 (0F B2 C3) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there.
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb2, 0xc3]);
    memory[0x18] = 0xee; // vector 6 IP low byte (IP = 0x00ee)
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lfs_with_register_operand_delivers_ud() {
    // lfs bx, ax (0F B4 C3, mod=3) -> #UD (vector 6).
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb4, 0xc3]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lgs_with_register_operand_delivers_ud() {
    // lgs bx, ax (0F B5 C3, mod=3) -> #UD (vector 6).
    let mut memory = vec![0u8; 1024];
    memory[0..3].copy_from_slice(&[0x0f, 0xb5, 0xc3]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
}

#[test]
fn lss_null_selector_faults_general_protection() {
    // In protected mode, LSS with a null selector must #GP -- SS can never be null,
    // the same rule any other SS load enforces (`null_selector_into_ss_still_faults`).
    let (mut cpu, mut memory) = protected_cpu(&[0x0f, 0xb2, 0x1e, 0x80, 0x01], 0, 0);
    // Far pointer at 0x180: offset 0x1234, selector 0x0000 (null).
    memory[0x180..0x182].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x182..0x184].copy_from_slice(&0x0000u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "a null selector into SS via LSS must #GP(0), got {fault:?}"
    );
}

#[test]
fn lss_protected_mode_refreshes_the_ss_b_bit() {
    // Load SS via LSS from a 32-bit (B=1) data descriptor (selector 0x08, GDT). The cached
    // `default_size_32` (the B bit) must flip to match the new descriptor -- it comes free
    // through `load_segment` -> `descriptor_to_segment`, exactly like any other segment load.
    let descriptor_low = 0x0000_ffffu32; // limit low = 0xffff, base = 0
    let descriptor_high = 0x00cf_9200u32; // access=0x92 (present, data, writable), B=1, G=1
    let (mut cpu, mut memory) = protected_cpu(
        &[0x0f, 0xb2, 0x1e, 0x80, 0x01],
        descriptor_low,
        descriptor_high,
    );
    assert!(
        !cpu.registers.segment(SegmentIndex::Ss).default_size_32,
        "test setup: SS must start 16-bit (B=0) so the flip is observable"
    );
    // Far pointer at 0x180: offset 0x1234, selector 0x08.
    memory[0x180..0x182].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x182..0x184].copy_from_slice(&0x0008u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x0008);
    assert!(
        cpu.registers.segment(SegmentIndex::Ss).default_size_32,
        "LSS must refresh SS.B to the loaded descriptor's B bit"
    );
}

#[test]
fn cbw_sign_extends_al_into_ax() {
    // cbw (0x98): al = 0x80 (-128) -> ax = 0xff80.
    let mut memory = vec![0; 64];
    memory[0] = 0x98;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
}

#[test]
fn cwde_sign_extends_ax_into_eax() {
    // 0x66 0x98 (CWDE): ax = 0x8000 -> eax = 0xffff_8000.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x98]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_8000);
}

#[test]
fn cwd_fills_dx_from_ax_sign() {
    // cwd (0x99): ax = 0x8000 (negative) -> dx = 0xffff, ax unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
}

#[test]
fn cwd_clears_dx_for_positive_ax() {
    // cwd (0x99): ax = 0x0001 (positive) -> dx = 0.
    let mut memory = vec![0; 64];
    memory[0] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Dx, 0xaaaa);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0000);
}

#[test]
fn cdq_fills_edx_from_eax_sign() {
    // 0x66 0x99 (CDQ): eax = 0x8000_0000 -> edx = 0xffff_ffff.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x99]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x8000_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.edx(), 0xffff_ffff);
}

#[test]
fn sti_sets_interrupt_flag() {
    // sti (0xfb) sets IF.
    let mut memory = vec![0; 64];
    memory[0] = 0xfb;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_IF));
}

#[test]
fn lahf_loads_flag_byte_into_ah() {
    // lahf (0x9f). CF=PF=AF=ZF=SF=1 -> AH = 0xD5 | 0x02 = 0xD7; AL unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x9f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_PF, true);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_ZF, true);
    cpu.set_flag(FLAG_SF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0xd7);
    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x00);
}

#[test]
fn sahf_loads_flags_from_ah_leaving_overflow() {
    // sahf (0x9e). AH=0xD7 -> CF=PF=AF=ZF=SF=1; OF untouched (a set OF survives).
    let mut memory = vec![0; 64];
    memory[0] = 0x9e;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xd700); // AH=0xD7, AL=0
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_PF, false);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_ZF, false);
    cpu.set_flag(FLAG_SF, false);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_PF));
    assert!(cpu.flag(FLAG_AF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_SF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn stc_sets_carry_and_cmc_toggles_it() {
    // stc (0xf9) sets CF; cmc (0xf5) toggles it back to 0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xf9, 0xf5]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // stc
    assert!(cpu.flag(FLAG_CF));

    cpu.cycle(&mut bus).unwrap(); // cmc
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn pushf_then_popf_restores_flags() {
    // pushf (0x9c) ; popf (0x9d). pushf saves CF=1; CF is perturbed by hand;
    // popf restores it and reserved bit 1 stays set.
    let mut memory = vec![0; 1024];
    memory[0] = 0x9c;
    memory[1] = 0x9d;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pushf
    cpu.set_flag(FLAG_CF, false); // perturb after the value is on the stack
    cpu.cycle(&mut bus).unwrap(); // popf

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.registers.eflags & 0x2, 0x2);
}

#[test]
fn leave_restores_sp_and_bp() {
    // leave (0xc9): sp <- bp; bp <- pop. bp = 0x0200, [ss:0x0200] = 0x1234.
    // Result: bp = 0x1234, sp = 0x0202 (0x0200 then +2 from the pop).
    let mut memory = vec![0; 1024];
    memory[0] = 0xc9;
    memory[0x200..0x202].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0080);
    cpu.write_gpr16(5, 0x0200); // BP
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr16(5), 0x1234);
    assert_eq!(cpu.read_gpr16(4), 0x0202);
}

#[test]
fn pusha_then_popa_round_trips_and_saves_original_sp() {
    // pusha (0x60) ; popa (0x61). All GPRs round-trip; the SP slot holds the
    // pre-pusha SP and popa discards it, so SP returns to its starting value.
    let mut memory = vec![0; 1024];
    memory[0] = 0x60;
    memory[1] = 0x61;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.write_gpr16(0, 0x1111);
    cpu.write_gpr16(1, 0x2222);
    cpu.write_gpr16(2, 0x3333);
    cpu.write_gpr16(3, 0x4444);
    cpu.write_gpr16(5, 0x6666);
    cpu.write_gpr16(6, 0x7777);
    cpu.write_gpr16(7, 0x8888);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pusha: 8 words, sp 0x0100 -> 0x00f0
    assert_eq!(cpu.read_gpr16(4), 0x00f0);
    // the 5th push (the SP slot) lands at 0x0100 - 2*5 = 0x00f6 and holds 0x0100
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xf6], bus.memory[0xf7]]),
        0x0100
    );

    cpu.cycle(&mut bus).unwrap(); // popa
    assert_eq!(cpu.read_gpr16(0), 0x1111);
    assert_eq!(cpu.read_gpr16(1), 0x2222);
    assert_eq!(cpu.read_gpr16(2), 0x3333);
    assert_eq!(cpu.read_gpr16(3), 0x4444);
    assert_eq!(cpu.read_gpr16(5), 0x6666);
    assert_eq!(cpu.read_gpr16(6), 0x7777);
    assert_eq!(cpu.read_gpr16(7), 0x8888);
    assert_eq!(cpu.read_gpr16(4), 0x0100);
}

#[test]
fn pushfd_pushes_only_defined_eflags_bits() {
    // 0x66 0x9c PUSHFD. EFLAGS carries garbage in the high bits; the 486 pushes
    // the defined low 16 plus AC (bit 18) and ID (bit 21). With every high bit
    // set in the source, the dword on the stack is 0x0024_0493.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.eflags = 0xfffc_0493;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    let pushed = u32::from_le_bytes([
        bus.memory[0xfc],
        bus.memory[0xfd],
        bus.memory[0xfe],
        bus.memory[0xff],
    ]);
    assert_eq!(pushed, 0x0024_0493);
    assert_eq!(cpu.registers.esp(), 0x0000_00fc);
}

#[test]
fn pushad_uses_16bit_sp_and_preserves_high_esp() {
    // 0x66 0x60 PUSHAD on a 16-bit stack: SP wraps within the segment and ESP[31:16]
    // is preserved. ESP = 0x0001_0010 -> SP 0x10 - 32 wraps to 0xfff0.
    let mut memory = vec![0; 0x2_0000];
    memory[0..2].copy_from_slice(&[0x66, 0x60]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0001_0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0001_fff0);
}

#[test]
fn popad_leaks_discarded_esp_high_half_on_16bit_stack() {
    // 0x66 0x61 POPAD on a 16-bit stack: the discarded saved-ESP slot's high half
    // lands in ESP[31:16] while SP keeps the advanced value (a 386 quirk).
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x61]);
    // The discard is the 4th dword, at SP + 12 = 0x20c.
    memory[0x20c..0x210].copy_from_slice(&0x5a04_6b18u32.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // SP 0x200 + 32 = 0x220; high half from the discarded slot = 0x5a04.
    assert_eq!(cpu.registers.esp(), 0x5a04_0220);
}

#[test]
fn pop_rm16_into_memory_disp16() {
    // 8F /0 with mod=00 rm=110 disp16: POP word [0x0200]. The encoding the
    // Wizardry III booter uses (with a CS override). Pops the stack top into
    // the memory word and advances SP by 2. Arithmetic flags are untouched.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x8f, 0x06, 0x00, 0x02]);
    // Stack top at ss:0x0100 = 0xbeef.
    memory[0x100..0x102].copy_from_slice(&0xbeefu16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.eflags = 0x0000_0ed7; // all arithmetic flags set
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0xbeef
    );
    assert_eq!(cpu.read_gpr16(4), 0x0102); // SP advanced by 2
    assert_eq!(cpu.registers.eflags, 0x0000_0ed7); // flags unchanged
}

#[test]
fn pop_rm16_into_register() {
    // 8F /0 with mod=11 rm=011: POP BX. Register destination form.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x8f, 0xc3]);
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_gpr16(4), 0x0102);
}

#[test]
fn pop_rm32_into_register_preserves_high_esp() {
    // 0x66 8F /0 mod=11 rm=001: POP ECX, 32-bit operand on a 16-bit stack.
    // The full dword loads into ECX; SP advances by 4 and ESP[31:16] is kept.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0x66, 0x8f, 0xc1]);
    memory[0x100..0x104].copy_from_slice(&0xcafe_f00du32.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xdead_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ecx(), 0xcafe_f00d);
    assert_eq!(cpu.registers.esp(), 0xdead_0104);
}

#[test]
fn pop_rm_reg_nonzero_is_illegal() {
    // 8F with reg != 0 is an illegal group encoding (group 1A reserves only /0), delivered as
    // a #UD through the real-mode IVT. Code is placed away from offset 0 so it doesn't
    // overlap the vector-0 IVT slot this test doesn't use, and vector 6's slot is populated
    // with a distinguishing trap address.
    const ORIGIN: usize = 0x10;
    const UD_TRAP_CS: u16 = 0x0300;
    const UD_TRAP_IP: u16 = 0x0020;
    let mut memory = vec![0; 1024];
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0x8f, 0xcb]); // mod=11 reg=001 rm=011
    memory[6 * 4..6 * 4 + 2].copy_from_slice(&UD_TRAP_IP.to_le_bytes());
    memory[6 * 4 + 2..6 * 4 + 4].copy_from_slice(&UD_TRAP_CS.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x0300);
    let mut bus = TestBus::with_memory(memory);

    let outcome = cpu
        .cycle(&mut bus)
        .expect("a delivered #UD must not error `cycle`");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, UD_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(UD_TRAP_IP));
}

#[test]
fn pop_rm32_esp_relative_destination_uses_post_increment_esp() {
    // The falsifier: push A; push B; pop dword [esp+4] must write B to the
    // POST-increment [esp+4] (the original pre-push top of stack, 0x0200) --
    // not the pre-pop [esp+4] (0x01fc, the slot that holds A), which is what
    // resolving the EA before the pop would compute.
    //
    // 0x66 0x67 8F /0 mod=01 rm=100 (SIB: base=ESP, no index) disp8=0x04:
    // POP dword [esp+4], 32-bit operand + 32-bit address override in real mode.
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0x66, 0x67, 0x8f, 0x44, 0x24, 0x04]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    // push A; push B (32-bit pushes on the 32-bit-addressed stack).
    cpu.push(&mut bus, 0xaaaa_aaaa, OperandSize::Dword).unwrap();
    cpu.push(&mut bus, 0xbbbb_bbbb, OperandSize::Dword).unwrap();
    assert_eq!(cpu.registers.esp(), 0x01f8);
    // Stack image: [0x01f8]=B (top), [0x01fc]=A.

    cpu.cycle(&mut bus).unwrap();

    // The pop reads B from 0x01f8 and advances esp to 0x01fc first; [esp+4]
    // computed AFTER that lands at 0x0200 (untouched before this instruction),
    // not at the pre-pop [esp+4] == 0x01fc (the slot holding A).
    assert_eq!(cpu.registers.esp(), 0x01fc, "pop advanced esp by 4");
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0200..0x0204].try_into().unwrap()),
        0xbbbb_bbbb,
        "post-increment EA wrote B to the pre-push top of stack, not A's slot"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01fc..0x0200].try_into().unwrap()),
        0xaaaa_aaaa,
        "A's slot is untouched: a pre-pop EA would have overwritten it with B"
    );
}

#[test]
fn pop_rm16_esp_relative_destination_uses_post_increment_esp() {
    // 16-bit variant of the falsifier above. 8F /0 mod=01 rm=100 (SIB: base=SP
    // is not directly encodable in 16-bit addressing -- 16-bit ModRM has no SIB
    // byte -- so this uses 32-bit addressing with a 16-bit operand: 0x67 8F /0
    // mod=01 rm=100 disp8=0x02, POP word [esp+2].
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x67, 0x8f, 0x44, 0x24, 0x02]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0xaaaa, OperandSize::Word).unwrap();
    cpu.push(&mut bus, 0xbbbb, OperandSize::Word).unwrap();
    assert_eq!(cpu.registers.esp(), 0x01fc);
    // Stack image: [0x01fc]=B (top), [0x01fe]=A.

    cpu.cycle(&mut bus).unwrap();

    // The pop reads B from 0x01fc and advances esp to 0x01fe first; [esp+2]
    // computed AFTER that lands at 0x0200 (untouched before this instruction),
    // not at the pre-pop [esp+2] == 0x01fe (the slot holding A).
    assert_eq!(cpu.registers.esp(), 0x01fe, "pop advanced esp by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x0200..0x0202].try_into().unwrap()),
        0xbbbb,
        "post-increment EA wrote B to the pre-push top of stack, not A's slot"
    );
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x01fe..0x0200].try_into().unwrap()),
        0xaaaa,
        "A's slot is untouched: a pre-pop EA would have overwritten it with B"
    );
}

#[test]
fn pop_rm32_esp_relative_destination_restores_esp_on_page_fault() {
    // (c) A faulting destination write must leave ESP exactly as it was before
    // the instruction started: the pop's ESP advance must be unwound so the
    // instruction is cleanly restartable after the guest's #PF handler fixes up
    // the mapping.
    //
    // PD at 0x1000, PT at 0x2000. Linear page 0 (code + the stack the pop reads
    // from) is identity-mapped present+writable. Linear page 0x3000 (where the
    // post-increment `[esp+4]` destination lands) has NO PTE at all, so the
    // destination write takes a #PF.
    let mut memory = vec![0; 0x4000];
    // Code at linear 0: POP dword [esp+4] (32-bit operand + 32-bit address
    // override in real mode).
    memory[0..6].copy_from_slice(&[0x66, 0x67, 0x8f, 0x44, 0x24, 0x04]);
    // The stack top read by the pop, at linear 0x2ffc: value B.
    memory[0x2ffc..0x3000].copy_from_slice(&0xbbbb_bbbbu32.to_le_bytes());
    // PDE[0] -> PT at 0x2000, present+rw+user.
    memory[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    // PTE[0] (linear 0x0000-0x0fff: code + the read side of the stack) -> identity, present+rw.
    memory[0x2000..0x2004].copy_from_slice(&0x0000_0007u32.to_le_bytes());
    // PTE[2] (linear 0x2000-0x2fff, covers 0x2ffc) -> identity, present+rw.
    memory[0x2008..0x200c].copy_from_slice(&0x0000_2007u32.to_le_bytes());
    // PTE[3] (linear 0x3000-0x3fff, the POP destination) intentionally left 0 (not present).
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.control.cr3 = 0x1000;
    cpu.control.cr0 |= CR0_PG;
    // ESP = 0x2ffc: the pop reads from here (mapped), advances to 0x3000, and
    // the destination EA [esp+4] (post-increment) is 0x3004 -- inside the
    // unmapped page 0x3000, so the write faults.
    cpu.registers.set_esp(0x2ffc);
    let esp_before = cpu.registers.esp();
    let mut bus = TestBus::with_memory(memory);

    // Use the raw decode/execute split (no exception delivery) so the assert
    // below observes ESP exactly as `execute_decoded` left it, not after a
    // real-mode #PF delivery has also pushed flags/CS/IP onto that same stack.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 14,
                error_code: Some(_)
            }
        ),
        "{fault:?}"
    );
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "a faulting destination write must leave esp exactly pre-instruction"
    );
}

#[test]
fn push_rm32_esp_source_reads_before_decrement() {
    // (d) PUSH r/m32 with an ESP-based memory source (JEMM's V86_MonitorEx
    // executes `push dword [esp]`) must read the source BEFORE the decrement:
    // the value pushed is the current top of stack, duplicating it, not
    // whatever ends up below the new top.
    //
    // 0x66 0x67 FF /6 mod=00 rm=100 (SIB: base=ESP, no index, no disp): PUSH
    // dword [esp], 32-bit operand + 32-bit address override in real mode.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x66, 0x67, 0xff, 0x34, 0x24]);
    memory[0x0200..0x0204].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x01fc, "push decremented esp by 4");
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01fc..0x0200].try_into().unwrap()),
        0xdead_beef,
        "the duplicated top-of-stack value, read before the decrement"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0200..0x0204].try_into().unwrap()),
        0xdead_beef,
        "the original top-of-stack slot is untouched"
    );
}

#[test]
fn pop_rm32_non_esp_base_is_unchanged() {
    // (e) A non-ESP base (EBX here) is unaffected by the pop-then-resolve
    // reorder: the destination EA never depended on ESP in the first place.
    //
    // 0x66 0x67 8F /0 mod=01 rm=011 disp8=0x10: POP dword [ebx+0x10].
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x66, 0x67, 0x8f, 0x43, 0x10]);
    memory[0x0100..0x0104].copy_from_slice(&0xcafe_babeu32.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.set_ebx(0x0300);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0104);
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x0310..0x0314].try_into().unwrap()),
        0xcafe_babe,
        "ebx+0x10 destination, unaffected by esp timing"
    );
}

#[test]
fn retf_pops_offset_then_segment() {
    // retf (0xcb). Stack at ss:0x0100 holds ip 0x0100 then cs 0x3000.
    let mut memory = vec![0; 1024];
    memory[0] = 0xcb;
    memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x0104); // two word pops from 0x0100
}

#[test]
fn far_call_then_retf_round_trips() {
    // call far 0x0000:0x0010 ; the target at 0x10 is retf (0xcb).
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x9a, 0x10, 0x00, 0x00, 0x00]);
    memory[0x10] = 0xcb;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // far call -> cs:0x0000, eip 0x0010
    assert_eq!(cpu.registers.eip, 0x0010);
    cpu.cycle(&mut bus).unwrap(); // retf -> back to cs:0x0000, eip 0x0005
    assert_eq!(cpu.registers.cs().selector, 0x0000);
    assert_eq!(cpu.registers.eip, 0x0005);
    assert_eq!(cpu.read_gpr16(4), 0x0100); // sp restored
}

#[test]
fn ret_near_imm16_pops_and_releases() {
    // ret 0x0004  (0xc2 0x04 0x00). Return ip 0x0100 at ss:0x0100.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xc2, 0x04, 0x00]);
    memory[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0100);
    // sp: 0x0100 -> +2 (word pop) -> +4 (release) = 0x0106
    assert_eq!(cpu.read_gpr16(4), 0x0106);
}

#[test]
fn ret_near_imm16_32bit_preserves_high_esp() {
    // 0x66 0xc2 0x04 0x00 : 32-bit ret, release 4. Pop eip (dword), then release.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x66, 0xc2, 0x04, 0x00]);
    memory[0x100..0x104].copy_from_slice(&0x0000_0100u32.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xdead_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0000_0100);
    // real-mode 16-bit stack: only SP moves, ESP[31:16] preserved.
    // 0x0100 -> +4 (dword pop) -> +4 (release) = 0x0108
    assert_eq!(cpu.registers.esp(), 0xdead_0108);
}

#[test]
fn retf_imm16_pops_far_and_releases() {
    // retf 0x0004  (0xca 0x04 0x00). Stack: ip 0x0100 then cs 0x3000 at ss:0x0100.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xca, 0x04, 0x00]);
    memory[0x100..0x104].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    // sp: 0x0100 -> +4 (far pop) -> +4 (release) = 0x0108
    assert_eq!(cpu.read_gpr16(4), 0x0108);
}

#[test]
fn release_stack_wraps_sp_and_preserves_high_esp_in_real_mode() {
    // release_stack alone, with no surrounding pop, must move only SP on a
    // real-mode 16-bit stack and wrap at the 16-bit boundary. ESP[31:16] must
    // not absorb the carry: a full-ESP add of 0xbeef_fffe + 4 would carry into
    // 0xbef0_0002, while the SP-only path gives 0xbeef_0002.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.set_esp(0xbeef_fffe);

    cpu.release_stack(4);

    assert_eq!(cpu.registers.esp(), 0xbeef_0002);
}

/// Load a protected-mode SS segment register directly (bypassing GDT resolution)
/// with the given B bit, for exercising `stack_is_32bit()` in isolation.
fn set_protected_ss(cpu: &mut Cpu386, base: u32, default_size_32: bool) {
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x10,
            base,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32,
        },
    );
}

#[test]
fn push_dword_on_a_16bit_protected_mode_stack_wraps_sp_and_preserves_high_esp() {
    // The DOS4GW/VCPI scenario: protected mode, a 32-bit push, but SS.B=0 (a
    // 16-bit stack segment). Only SP must wrap; ESP[31:16] survives untouched,
    // and the write lands at SS.base + the wrapped SP, not at SS.base + ESP.
    let memory = vec![0u8; 0x1_0002];
    let mut cpu = Cpu386::default();
    set_protected_ss(&mut cpu, 0, false);
    cpu.registers.set_esp(0xbeef_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0x1122_3344, OperandSize::Dword).unwrap();

    // sp 0x0002 -> wraps to 0xfffe; ESP high half (0xbeef) preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_fffe);
    let read = bus
        .read_memory_direct(0xfffe, BusWidth::Dword, BusAccessKind::DataRead)
        .unwrap();
    assert_eq!(read.value, 0x1122_3344);
}

#[test]
fn pop_dword_on_a_16bit_protected_mode_stack_wraps_sp_and_preserves_high_esp() {
    // Mirror of the push case: SS.B=0 in protected mode reads from the wrapped
    // SP and advances only SP, leaving ESP[31:16] alone.
    let mut memory = vec![0u8; 0x1_0002];
    memory[0xfffe..0x1_0002].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    let mut cpu = Cpu386::default();
    set_protected_ss(&mut cpu, 0, false);
    cpu.registers.set_esp(0xbeef_fffe);
    let mut bus = TestBus::with_memory(memory);

    let value = cpu.pop(&mut bus, OperandSize::Dword).unwrap();

    assert_eq!(value, 0x1122_3344);
    // sp 0xfffe -> +4 wraps to 0x0002; ESP high half preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_0002);
}

#[test]
fn push_dword_on_a_32bit_protected_mode_stack_uses_full_esp() {
    // SS.B=1 (the TOKAEMM monitor's stack shape): full-ESP arithmetic, no wrap
    // at the 16-bit boundary, matching today's protected-mode behavior.
    let memory = vec![0u8; 0x2_0000];
    let mut cpu = Cpu386::default();
    set_protected_ss(&mut cpu, 0, true);
    cpu.registers.set_esp(0x0001_0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.push(&mut bus, 0xaabb_ccdd, OperandSize::Dword).unwrap();

    assert_eq!(cpu.registers.esp(), 0x0000_fffe);
    let read = bus
        .read_memory_direct(0x0000_fffe, BusWidth::Dword, BusAccessKind::DataRead)
        .unwrap();
    assert_eq!(read.value, 0xaabb_ccdd);
}

#[test]
fn pop_dword_on_a_32bit_protected_mode_stack_uses_full_esp() {
    let mut memory = vec![0u8; 0x2_0000];
    memory[0x0000_fffe..0x0001_0002].copy_from_slice(&0xaabb_ccddu32.to_le_bytes());
    let mut cpu = Cpu386::default();
    set_protected_ss(&mut cpu, 0, true);
    cpu.registers.set_esp(0x0000_fffe);
    let mut bus = TestBus::with_memory(memory);

    let value = cpu.pop(&mut bus, OperandSize::Dword).unwrap();

    assert_eq!(value, 0xaabb_ccdd);
    assert_eq!(cpu.registers.esp(), 0x0001_0002);
}

#[test]
fn ss_load_populates_the_cached_b_bit_from_the_descriptor() {
    // A GDT-resolved SS load must cache B from descriptor bit 22, and a
    // subsequent real-mode load must clear it back to false.
    let mut memory = vec![0u8; 4096];
    // GDT at 0, entry 1 (selector 0x08): base 0, limit 0xfffff (4K gran), B=1
    // data segment, present, DPL 0. Access byte 0x93 (present, data, r/w).
    // High dword: limit high nibble 0xf | G=1,D/B=1 -> 0xc0 | 0x0f = 0xcf in bits 16-23,
    // access in bits 8-15.
    let low: u32 = 0xffff; // limit low
    let high: u32 = 0x00cf_9300u32; // G=1,B=1,limit_high=0xf,access=0x93
    memory[8..12].copy_from_slice(&low.to_le_bytes());
    memory[12..16].copy_from_slice(&high.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.gdtr.base = 0;
    cpu.gdtr.limit = 0xffff;
    let mut bus = TestBus::with_memory(memory);

    cpu.load_segment(&mut bus, SegmentIndex::Ss, 0x08).unwrap();
    assert!(cpu.stack_is_32bit());

    cpu.load_segment_real(SegmentIndex::Ss, 0);
    assert!(!cpu.stack_is_32bit());
}

/// A flat protected-mode CPU with code at linear 0, CS.D from `code_d32`, and SS.B
/// from `stack_b32` -- for exercising ENTER/LEAVE's SS.B-vs-operand-size split.
fn protected_cpu_with_cs_d_and_ss_b(
    code: &[u8],
    mem_len: usize,
    code_d32: bool,
    stack_b32: bool,
) -> (Cpu386, Vec<u8>) {
    let mut memory = vec![0u8; mem_len];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x08,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: code_d32,
        },
    );
    set_protected_ss(&mut cpu, 0, stack_b32);
    cpu.registers.eip = 0;
    (cpu, memory)
}

#[test]
fn leave_on_a_32bit_stack_moves_full_esp_even_with_a_16bit_operand_size() {
    // LEAVE with a 0x66 operand-size prefix (word EBP/BP pop) on an SS.B=1 stack.
    // Per PRM 17-96, StackAddrSize=32 => ESP <- EBP unconditionally: the full
    // register, not the low word, regardless of the operand size. EBP carries a
    // high word (0x0002) distinct from ESP's stale high word (0xdead) that must
    // land in ESP whole; a truncating write would leave ESP's stale 0xdead high
    // half instead of EBP's 0x0002.
    let (mut cpu, memory) = protected_cpu_with_cs_d_and_ss_b(&[0x66, 0xc9], 0x3_0000, true, true);
    cpu.write_gpr32(5, 0x0002_0100); // EBP
    cpu.registers.set_esp(0xdead_0000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        cpu.registers.esp(),
        0x0002_0100 + 2,
        "ESP <- full EBP, then +2 from the 16-bit pop"
    );
}

#[test]
fn leave_on_a_16bit_stack_moves_only_sp_and_preserves_high_esp() {
    // Mirror on an SS.B=0 stack (real mode's rule, still true in protected mode):
    // only SP takes BP's value; ESP's high word is untouched.
    let (mut cpu, memory) = protected_cpu_with_cs_d_and_ss_b(&[0xc9], 0x1_0000, false, false);
    cpu.write_gpr16(5, 0x0200); // BP
    cpu.registers.set_esp(0xbeef_0080);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // SP <- 0x0200, then +2 from the pop = 0x0202; high half preserved.
    assert_eq!(cpu.registers.esp(), 0xbeef_0202);
}

#[test]
fn enter_op32_on_a_16bit_stack_saves_frame_ptr_from_sp_not_esp() {
    // ENTER imm16,1 (op32) on an SS.B=0 stack: frame-ptr <- eSP is the 16-bit SP
    // (386 PRM 17-62), not the full (garbage-laden) ESP. With nesting level 1 the
    // frame-ptr is pushed once more, so the pushed dword must carry the wrapped SP
    // zero-extended, not ESP's high garbage.
    let (mut cpu, memory) =
        protected_cpu_with_cs_d_and_ss_b(&[0xc8, 0x04, 0x00, 0x01], 0x1_0000, true, false);
    cpu.registers.set_esp(0xbeef_0100);
    cpu.write_gpr32(5, 0); // EBP, arbitrary
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // Push(EBP) at SP 0x0100 -> SP=0x00fc; frame-ptr = SP = 0x00fc (not
    // 0xbeef_00fc); level>0 so frame-ptr is pushed again at SP=0x00f8; final
    // alloc SP -= 4 = 0x00f4. High half of ESP preserved throughout.
    let frame_ptr_slot = u32::from_le_bytes(bus.memory[0xf8..0xfc].try_into().unwrap());
    assert_eq!(
        frame_ptr_slot, 0x00fc,
        "pushed frame-ptr is the 16-bit SP, zero-extended, not ESP-high garbage"
    );
    assert_eq!(cpu.registers.esp(), 0xbeef_00f4);
    assert_eq!(
        cpu.read_gpr32(5),
        0x00fc,
        "EBP <- frame-ptr (zero-extended)"
    );
}

#[test]
fn far_call_pushes_return_and_loads_target() {
    // call far 0x3000:0x0100  (0x9a 0x00 0x01 0x00 0x30), a 5-byte instruction.
    // Pushes CS (0x0000) then the return IP (0x0005), then loads cs:eip.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0x9a, 0x00, 0x01, 0x00, 0x30]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc); // two word pushes from 0x0100
    // CS at the higher slot, return IP just below it
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0000
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
        0x0005
    );
}

#[test]
fn far_call_via_memory_pushes_return_and_transfers() {
    // call far [0x0200]  (0xff 0x1e 0x00 0x02), a 4-byte instruction. The far
    // pointer at ds:0x0200 is offset 0x0100, selector 0x3000.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0xff, 0x1e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc);
    // return CS 0x0000 at the higher slot, return IP 0x0004 below it
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0x0000
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfc], bus.memory[0xfd]]),
        0x0004
    );
}

#[test]
fn far_jmp_via_memory_transfers_without_pushing() {
    // jmp far [0x0200]  (0xff 0x2e 0x00 0x02). Pointer = offset 0x0100, selector 0x3000.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0xff, 0x2e, 0x00, 0x02]);
    memory[0x200..0x204].copy_from_slice(&[0x00, 0x01, 0x00, 0x30]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x0100); // nothing pushed
}

#[test]
fn far_call_via_register_operand_delivers_ud() {
    // 0xff /3 with mod=3 (0xff 0xd8) is an invalid encoding -> #UD (vector 6).
    // IVT[6] at 0x18 points to IP 0x00ee, CS 0; the CPU vectors there and clears IF.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn far_jmp_via_register_operand_delivers_ud() {
    // 0xff /5 with mod=3 (0xff 0xe8) is an invalid encoding -> #UD (vector 6).
    // The conformance suite pre-skips exception vectors, so this is the only
    // guard that the register form of the far JMP faults rather than transfers.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0xff, 0xe8]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn far_call_via_memory_wraps_selector_offset_at_64k() {
    // call far [bx+di] (0xff 0x19) with bx+di = 0xfffe. On a 16-bit real-mode
    // segment the IP is read at ds:0xfffe and the selector offset wraps to
    // ds:0x0000 rather than reading past the 0xffff limit; a real 80386
    // completes this without faulting (SingleStepTests FF.3 "call far
    // [ds:bx+di]" with bx=di=0xffff).
    let ds_base = 0x2_0000usize; // ds selector 0x2000
    let mut memory = vec![0; 0x3_0000];
    memory[0..2].copy_from_slice(&[0xff, 0x19]);
    // IP at ds:0xfffe
    memory[ds_base + 0xfffe..ds_base + 0x1_0000].copy_from_slice(&0x0100u16.to_le_bytes());
    // selector at the wrapped ds:0x0000
    memory[ds_base..ds_base + 2].copy_from_slice(&0x3000u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0x2000);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.write_gpr16(3, 0xfffe); // bx
    cpu.write_gpr16(7, 0x0000); // di
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.read_gpr16(4), 0x00fc); // pushed CS then return IP
}

#[test]
fn retf_32bit_pops_full_eip_and_preserves_high_esp() {
    // 0x66 0xcb (32-bit RETF). Pops EIP (dword, not masked to 16) then CS
    // (dword, truncated to the selector). On the real-mode 16-bit stack only
    // SP moves, so ESP[31:16] is preserved.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0xcb]);
    memory[0x100..0x104].copy_from_slice(&0x0001_2345u32.to_le_bytes()); // EIP
    memory[0x104..0x108].copy_from_slice(&0x0000_3000u32.to_le_bytes()); // CS
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0xcafe_0100);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x3000);
    assert_eq!(cpu.registers.eip, 0x0001_2345);
    // sp 0x0100 -> +8 (two dword pops) = 0x0108, high half preserved
    assert_eq!(cpu.registers.esp(), 0xcafe_0108);
}

#[test]
fn movzx_byte_zero_extends_into_ax() {
    // movzx ax, bl  (0x0f 0xb6 0xc3, modrm mod=3 reg=ax rm=bl): bl=0x80 -> ax=0x0080.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb6, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80); // bl
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
}

#[test]
fn movzx_byte_zero_extends_into_eax_clearing_high_bits() {
    // 0x66 0x0f 0xb6 0xc3 (movzx eax, bl): bl=0x80, eax preset 0xffff_ffff -> eax=0x0000_0080.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb6, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xffff_ffff);
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_0080);
}

#[test]
fn movzx_word_zero_extends_into_eax() {
    // 0x66 0x0f 0xb7 0xc3 (movzx eax, bx): bx=0x8000, eax preset 0xffff_ffff -> eax=0x0000_8000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb7, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xffff_ffff);
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0x0000_8000);
}

#[test]
fn movsx_byte_sign_extends_into_ax() {
    // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x80 -> ax=0xff80.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xff80);
}

#[test]
fn movsx_byte_sign_extends_into_eax() {
    // 0x66 0x0f 0xbe 0xc3 (movsx eax, bl): bl=0x80 -> eax=0xffff_ff80.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbe, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x80);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_ff80);
}

#[test]
fn movsx_word_sign_extends_into_eax() {
    // 0x66 0x0f 0xbf 0xc3 (movsx eax, bx): bx=0x8000 -> eax=0xffff_8000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbf, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xffff_8000);
}

#[test]
fn movsx_byte_positive_source_zero_fills() {
    // movsx ax, bl (0x0f 0xbe 0xc3): bl=0x7f (positive) -> ax=0x007f, no sign fill.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbe, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0x7f);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x007f);
}

#[test]
fn movzx_word_into_16bit_dest_preserves_high_eax() {
    // movzx ax, bx (0x0f 0xb7 0xc3, no 0x66): a word source into a 16-bit
    // destination is a plain word move; the high half of EAX is preserved by
    // write_gpr16.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb7, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xdead_0000);
    cpu.write_reg16(Reg16::Bx, 0x8000);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eax(), 0xdead_8000);
}

#[test]
fn movzx_reads_byte_from_memory_source() {
    // movzx ax, byte [0x40] (0x0f 0xb6 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
    // [ds:0x40]=0x80 -> ax=0x0080. Exercises the memory-source decode path that the
    // register-operand tests do not, since the conformance vectors are not in CI.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xb6, 0x06, 0x40, 0x00]);
    memory[0x40] = 0x80;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0080);
}

#[test]
fn setz_sets_byte_when_zf_set() {
    // setz bl (0x0f 0x94 0xc3): ZF=1 -> bl=1. bl preset 0xff to prove it is overwritten.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0xff);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 1);
    // SETcc writes the byte without disturbing the flag it tested.
    assert!(cpu.flag(FLAG_ZF));
}

#[test]
fn setz_clears_byte_when_zf_clear() {
    // setz bl (0x0f 0x94 0xc3): ZF=0 -> bl=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0x94, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(3, 0xff);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 0);
}

#[test]
fn setnz_writes_memory_destination() {
    // setnz byte [0x40] (0x0f 0x95 0x06 0x40 0x00, modrm mod=00 rm=110 disp16):
    // ZF=0 -> !ZF true -> [ds:0x40]=1.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0x95, 0x06, 0x40, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 1);
}

#[test]
fn imul_0f_af_16bit_fits_clears_carry_overflow() {
    // imul bx, cx (0x0f 0xaf 0xd9, modrm mod=3 reg=bx rm=cx): 3 * 4 = 12, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.write_reg16(Reg16::Cx, 4);
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 12);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_16bit_overflow_sets_carry_overflow() {
    // imul bx, cx (0x0f 0xaf 0xd9): 0x1000 * 0x10 = 0x10000, truncates to 0, CF=OF=1.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x1000);
    cpu.write_reg16(Reg16::Cx, 0x0010);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_32bit_fits_clears_carry_overflow() {
    // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 1000 * 1000 = 1_000_000, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(3, 1000); // ebx
    cpu.write_gpr32(1, 1000); // ecx
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 1_000_000);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_32bit_overflow_sets_carry_overflow() {
    // 0x66 0x0f 0xaf 0xd9 (imul ebx, ecx): 0x10000 * 0x10000 = 0x1_0000_0000,
    // truncates to 0, CF=OF=1.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(3, 0x0001_0000); // ebx
    cpu.write_gpr32(1, 0x0001_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 0x0000_0000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_signed_negative_result_fits() {
    // imul bx, cx (0x0f 0xaf 0xd9): -1 * 5 = -5 (0xfffb), fits signed 16-bit, CF=OF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.write_reg16(Reg16::Cx, 0x0005);
    cpu.set_flag(FLAG_CF | FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xfffb);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn imul_0f_af_signed_overflow_differs_from_unsigned() {
    // imul bx, cx (0x0f 0xaf 0xd9): -1 * -32768 = +32768. The low half 0x8000
    // sign-extends to -32768, not +32768, so the signed result does not fit:
    // bx=0x8000, CF=OF=1. An unsigned multiply of 0xffff * 0x8000 would truncate
    // to the same 0x8000 but read as non-overflowing, so this distinguishes IMUL.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xaf, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.write_reg16(Reg16::Cx, 0x8000); // -32768
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x8000);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn bsf_finds_lowest_set_bit() {
    // bsf bx, cx (0x0f 0xbc 0xd9): cx=0x0140 -> lowest set bit at index 6, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0140);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 6);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsf_zero_source_sets_zf_and_leaves_dest() {
    // bsf bx, cx (0x0f 0xbc 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0xbeef).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbc, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 0xbeef);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xbeef);
}

#[test]
fn bsf_32bit_finds_low_bit() {
    // 0x66 0x0f 0xbc 0xd9 (bsf ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbc, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0x8000_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 31); // ebx
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_finds_highest_set_bit() {
    // bsr bx, cx (0x0f 0xbd 0xd9): cx=0x0140 -> highest set bit at index 8, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0140);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Bx), 8);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_32bit_finds_high_bit() {
    // 0x66 0x0f 0xbd 0xd9 (bsr ebx, ecx): ecx=0x8000_0000 -> index 31, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xbd, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0x8000_0000); // ecx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(3), 31); // ebx
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn bsr_zero_source_sets_zf_and_leaves_dest() {
    // bsr bx, cx (0x0f 0xbd 0xd9): cx=0 -> ZF=1, bx unchanged (preset 0x1234).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbd, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
}

#[test]
fn bt_register_reads_set_bit() {
    // bt cx, bx (0x0f 0xa3 0xd9, modrm mod=3 reg=bx rm=cx): cx=0x0008 bit 3, bx=3 -> CF=1, cx unchanged.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
}

#[test]
fn bt_register_reads_clear_bit() {
    // bt cx, bx: cx=0x0008, bx=2 (bit 2 clear) -> CF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa3, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 2);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn bts_register_sets_bit_and_reads_old() {
    // bts cx, bx (0x0f 0xab 0xd9): cx=0x0000, bx=3 -> CF=0 (old bit), cx=0x0008.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xab, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0000);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0008);
}

#[test]
fn btr_register_clears_bit_and_reads_old() {
    // btr cx, bx (0x0f 0xb3 0xd9): cx=0x0008, bx=3 -> CF=1 (old bit), cx=0x0000.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xb3, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn btc_register_toggles_bit() {
    // btc cx, bx (0x0f 0xbb 0xd9): cx=0x0008, bx=3 -> CF=1 (old), cx=0x0000 (toggled off).
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xbb, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Bx, 3);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn bts_memory_positive_index_walks_to_next_word() {
    // bts [0x40], bx (0x0f 0xab 0x1e 0x40 0x00, modrm mod=00 reg=bx rm=110 disp16):
    // bx=17 -> block 1, bit 1 -> word at 0x42 (0x40+2). [0x42]=0 -> CF=0, [0x42]=0x0002.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xab, 0x1e, 0x40, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 17);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x42], bus.memory[0x43]]),
        0x0002
    );
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0000
    );
}

#[test]
fn bt_memory_negative_index_walks_to_previous_word() {
    // bt [0x40], bx (0x0f 0xa3 0x1e 0x40 0x00): bx=0xffff (-1) -> block -1, bit 15 ->
    // word at 0x3e (0x40-2). [0x3e]=0x8000 -> CF=1. BT does not write.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xa3, 0x1e, 0x40, 0x00]);
    memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
        0x8000
    );
}

#[test]
fn btc_memory_negative_index_walks_and_toggles() {
    // btc [0x40], bx (0x0f 0xbb 0x1e 0x40 0x00): bx=0xffff (-1) -> word at 0x3e, bit 15.
    // [0x3e]=0x8000 (bit 15 set) -> CF=1, the bit toggles off -> [0x3e]=0x0000.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0xbb, 0x1e, 0x40, 0x00]);
    memory[0x3e..0x40].copy_from_slice(&0x8000u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff); // -1
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x3e], bus.memory[0x3f]]),
        0x0000
    );
}

#[test]
fn bts_32bit_register_sets_high_bit() {
    // 0x66 0x0f 0xab 0xd9 (bts ecx, ebx): ecx=0, ebx=20 -> CF=0, ecx bit 20 set.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xab, 0xd9]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0); // ecx
    cpu.write_gpr32(3, 20); // ebx
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_gpr32(1), 0x0010_0000);
}

#[test]
fn bt_immediate_reads_selected_bit() {
    // bt cx, 5 (0x0f 0xba 0xe1 0x05, modrm mod=3 reg=/4 rm=cx): cx=0x0020 bit 5 -> CF=1, cx unchanged.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xe1, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0020);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0020);
}

#[test]
fn btr_immediate_clears_selected_bit() {
    // btr cx, 5 (0x0f 0xba 0xf1 0x05, modrm mod=3 reg=/6 rm=cx): cx=0x0020 -> CF=1, cx=0x0000.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xf1, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0x0020);
    cpu.set_flag(FLAG_CF, false); // prove CF=1 comes from the old bit, not a residual
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_CF));
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x0000);
}

#[test]
fn bts_immediate_memory_no_walk() {
    // bts [0x40], 5 (0x0f 0xba 0x2e 0x40 0x00 0x05, modrm mod=00 reg=/5 rm=110 disp16):
    // imm bit 5, accesses [0x40] directly (no walk). [0x40]=0 -> CF=0, [0x40]=0x0020.
    let mut memory = vec![0; 128];
    memory[0..6].copy_from_slice(&[0x0f, 0xba, 0x2e, 0x40, 0x00, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_CF));
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0020
    );
}

#[test]
fn bt_immediate_reg_below_4_delivers_ud() {
    // 0x0f 0xba 0xc1 0x05 (modrm mod=3 reg=/0 rm=cx): reg<4 is invalid -> #UD (vector 6).
    // 1024 bytes so the stack push at 0x0100 (6 bytes) and IVT at 0x18 both fit.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x0f, 0xba, 0xc1, 0x05]);
    memory[0x18] = 0xee; // IVT[6] IP low byte -> IP 0x00ee
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn shld_imm_shifts_left_and_fills_from_source() {
    // shld ax, bx, 4 (0x0f 0xa4 0xd8 0x04, modrm mod=3 reg=bx rm=ax):
    // ax=0x1234, bx=0x5678 -> ax=0x2345, CF=1 (bit shifted out of ax bit 12).
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x04]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
    assert!(cpu.flag(FLAG_CF));
}

#[test]
fn shrd_imm_shifts_right_and_fills_from_source() {
    // shrd ax, bx, 4 (0x0f 0xac 0xd8 0x04): ax=0x1234, bx=0x5678 -> ax=0x8123, CF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x04]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_cl_uses_cl_count() {
    // shld ax, bx, cl (0x0f 0xa5 0xd8): cl=4 -> same as imm 4: ax=0x2345, CF=1.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xa5, 0xd8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x2345);
    assert!(cpu.flag(FLAG_CF));
}

#[test]
fn shrd_cl_uses_cl_count() {
    // shrd ax, bx, cl (0x0f 0xad 0xd8): cl=4 -> same as shrd imm 4: ax=0x8123, CF=0.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x0f, 0xad, 0xd8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.write_reg16(Reg16::Cx, 0x0004); // cl = 4
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8123);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_32bit_imm() {
    // 0x66 0x0f 0xa4 0xd8 0x08 (shld eax, ebx, 8): eax=0x1234_5678, ebx=0x9abc_def0
    // -> eax=0x3456_789a, CF=0.
    let mut memory = vec![0; 64];
    memory[0..5].copy_from_slice(&[0x66, 0x0f, 0xa4, 0xd8, 0x08]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x1234_5678); // eax
    cpu.write_gpr32(3, 0x9abc_def0); // ebx
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x3456_789a);
    assert!(!cpu.flag(FLAG_CF));
}

#[test]
fn shld_count_one_sets_overflow_on_sign_change() {
    // shld ax, bx, 1 (0x0f 0xa4 0xd8 0x01): ax=0x4000 -> ax=0x8000, sign flips, OF=1.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x4000);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    cpu.set_flag(FLAG_OF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x8000);
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_one_clears_overflow_without_sign_change() {
    // shld ax, bx, 1: ax=0x0001 -> ax=0x0002, sign unchanged, OF=0.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x01]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0002);
    assert!(!cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_zero_is_noop() {
    // shld ax, bx, 0 (0x0f 0xa4 0xd8 0x00): ax unchanged, flags unchanged.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_OF));
}

#[test]
fn shld_count_past_width_rotates_source() {
    // shld ax, bx, 18 (0x0f 0xa4 0xd8 0x12): count 18 > 16 is undefined per Intel; the
    // 386 leaves ax as the source rotated left by 18 mod 16 = 2. bx=0x1234 -> ax=0x48d0.
    // The destination's prior value does not matter (preset 0xffff).
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xa4, 0xd8, 0x12]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x48d0);
}

#[test]
fn shrd_count_past_width_rotates_source() {
    // shrd ax, bx, 18 (0x0f 0xac 0xd8 0x12): the 386 leaves ax as the source rotated
    // right by 2. bx=0x1234 -> ax=0x048d.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0xac, 0xd8, 0x12]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Bx, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x048d);
}

#[test]
fn xchg_byte_swaps_registers() {
    // xchg al, bl (0x86 0xc3, modrm mod=3 reg=al rm=bl). al=0x12, bl=0x34 -> al=0x34, bl=0x12.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x86, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0012);
    cpu.write_reg16(Reg16::Bx, 0x0034);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x34);
    assert_eq!(cpu.read_reg16(Reg16::Bx) & 0xff, 0x12);
}

#[test]
fn xchg_word_swaps_registers() {
    // xchg bx, ax (0x87 0xc3, modrm reg=ax rm=bx). ax=0x1234, bx=0x5678 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x87, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Bx, 0x5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
}

#[test]
fn xchg_word_swaps_register_and_memory() {
    // xchg [0x40], ax (0x87 0x06 0x40 0x00, modrm mod=0 reg=ax rm=110 disp16).
    let mut memory = vec![0; 128];
    memory[0..4].copy_from_slice(&[0x87, 0x06, 0x40, 0x00]);
    memory[0x40] = 0xcd;
    memory[0x41] = 0xab; // word at 0x40 = 0xabcd
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xabcd);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x1234
    );
}

#[test]
fn xchg_dword_swaps_registers() {
    // 0x66 0x87 0xc3 (xchg ebx, eax). eax=0x1111_2222, ebx=0x3333_4444 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x66, 0x87, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x1111_2222);
    cpu.write_gpr32(3, 0x3333_4444);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x3333_4444);
    assert_eq!(cpu.read_gpr32(3), 0x1111_2222);
}

#[test]
fn xchg_accumulator_swaps_ax_with_reg() {
    // xchg ax, cx (0x91). ax=0x1234, cx=0x5678 -> swapped.
    let mut memory = vec![0; 64];
    memory[0] = 0x91;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Cx, 0x5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x5678);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x1234);
}

#[test]
fn xchg_accumulator_dword_swaps_eax_with_reg() {
    // 0x66 0x93 (xchg eax, ebx). eax=0x0001_0002, ebx=0x0003_0004 -> swapped.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x66, 0x93]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(0, 0x0001_0002);
    cpu.write_gpr32(3, 0x0003_0004);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x0003_0004);
    assert_eq!(cpu.read_gpr32(3), 0x0001_0002);
}

#[test]
fn xchg_byte_swaps_register_and_memory_with_displacement() {
    // xchg [bx+0x10], al (0x86 0x47 0x10, modrm mod=1 reg=al rm=[bx]+disp8).
    // bx=0x20 -> address 0x30. Guards against re-decoding the ModRm, which would
    // consume a second displacement byte and advance eip past the instruction.
    let mut memory = vec![0; 128];
    memory[0..3].copy_from_slice(&[0x86, 0x47, 0x10]);
    memory[0x30] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0020);
    cpu.write_reg16(Reg16::Ax, 0x0055); // AL = 0x55
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99); // AL got the memory byte
    assert_eq!(bus.memory[0x30], 0x55); // memory got AL
    assert_eq!(cpu.registers.eip, 3); // opcode + modrm + disp8, no extra fetch
}

#[test]
fn loopne_decrements_cx_and_branches_while_not_equal() {
    // loopne +5 (0xe0 0x05). cx=3, ZF=0 -> cx=2, taken: eip = 2 + 5 = 7.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn loopne_falls_through_when_zero_flag_set() {
    // loopne +5: cx=3, ZF=1 -> cx=2, not taken (LOOPNE loops while ZF=0): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn loopne_falls_through_when_count_reaches_zero() {
    // loopne +5: cx=1, ZF=0 -> cx=0, not taken (count zero): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe0, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 1);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn loope_branches_while_equal() {
    // loope +5 (0xe1 0x05): cx=3, ZF=1 -> cx=2, taken: eip = 7.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe1, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn loope_falls_through_when_zero_flag_clear() {
    // loope +5 (0xe1 0x05): cx=3, ZF=0 -> cx=2, not taken (LOOPE loops while ZF=1): eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe1, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 3);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 2);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn jcxz_branches_only_when_cx_zero() {
    // jcxz +5 (0xe3 0x05): cx=0 -> taken (eip=7), no decrement.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe3, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.registers.eip, 7);
}

#[test]
fn jcxz_falls_through_when_cx_nonzero() {
    // jcxz +5: cx=1 -> not taken: eip = 2.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xe3, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Cx, 1);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Cx), 1);
    assert_eq!(cpu.registers.eip, 2);
}

#[test]
fn jecxz_uses_ecx_with_address_override() {
    // 0x67 jecxz +5 (0x67 0xe3 0x05): ecx=0 -> taken: eip = 3 + 5 = 8.
    let mut memory = vec![0; 64];
    memory[0..3].copy_from_slice(&[0x67, 0xe3, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr32(1, 0); // ecx = 0
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 8);
    assert_eq!(cpu.registers.ecx(), 0); // JECXZ does not decrement
}

#[test]
fn xlat_reads_ds_table_indexed_by_al() {
    // xlat (0xd7): DS:0, BX=0x10, AL=0x05 -> AL = [0x15].
    let mut memory = vec![0; 64];
    memory[0] = 0xd7;
    memory[0x15] = 0xab;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Ax, 0x0005); // AL = 5, AH = 0
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xab);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00); // AH unchanged
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x0010); // BX unchanged
}

#[test]
fn xlat_wraps_the_16bit_base_plus_index() {
    // xlat: BX=0xffff, AL=0x02 -> offset = (0xffff + 2) & 0xffff = 0x0001.
    let mut memory = vec![0; 64];
    memory[0] = 0xd7;
    memory[0x01] = 0xcd;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0xffff);
    cpu.write_reg16(Reg16::Ax, 0x0002);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xcd);
}

#[test]
fn xlat_honours_a_segment_override() {
    // 0x26 xlat (es override). ES base = 0x0100 << 4 = 0x1000. BX=0x10, AL=0x05 -> [0x1015].
    let mut memory = vec![0; 0x2000];
    memory[0..2].copy_from_slice(&[0x26, 0xd7]);
    memory[0x1015] = 0x99;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0x0100);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x99);
}

#[test]
fn daa_low_nibble_correction() {
    // daa (0x27): AL=0x7C, CF=0, AF=0 -> AL=0x82 (low nibble +6), CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x007c);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x82);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn daa_both_corrections_set_carry() {
    // daa: AL=0xAA -> +6 = 0xB0 (AF=1), then +0x60 = 0x10 (CF=1).
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x00aa);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x10);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn daa_incoming_aux_carry_triggers_correction() {
    // daa: AL=0x20 (low nibble <= 9), AF=1 -> the first correction fires on AF alone:
    // AL=0x26, CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x27;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0020);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x26);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn das_low_nibble_correction() {
    // das (0x2f): AL=0x4A, CF=0, AF=0 -> AL=0x44 (low nibble -6), CF=0, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x2f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x004a);
    cpu.set_flag(FLAG_CF, false);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x44);
    assert!(!cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn das_high_correction_on_incoming_carry() {
    // das: AL=0x00, CF=1, AF=0 -> -0x60 = 0xA0, CF=1, AF=0.
    let mut memory = vec![0; 64];
    memory[0] = 0x2f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.set_flag(FLAG_CF, true);
    cpu.set_flag(FLAG_AF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0xa0);
    assert!(cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aaa_adjusts_and_carries_into_ah() {
    // aaa (0x37): AX=0x000B (AL low nibble > 9) -> AX += 0x106, AL &= 0x0f.
    // AX=0x0111 then AL=0x01 -> AX=0x0101; CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x000b);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x01);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aaa_no_adjust_clears_carry() {
    // aaa: AX=0x0005, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aas_adjusts_and_borrows_from_ah() {
    // aas (0x3f): AX=0x020B (AL low nibble > 9) -> AX -= 6, AH -= 1, AL &= 0x0f.
    // 0x020B - 6 = 0x0205, AH-1 -> 0x0105, AL=0x05; CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x020b);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aas_no_adjust_clears_carry() {
    // aas: AX=0x0204, AF=0 -> only AL &= 0x0f; CF=0, AF=0, AH unchanged.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0204);
    cpu.set_flag(FLAG_AF, false);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x04);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x02);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_AF));
}

#[test]
fn aaa_aux_carry_triggers_adjust() {
    // aaa: AL=0x01 (low nibble <= 9), AF=1 -> the adjust fires on AF alone.
    // AX=0x0001 + 0x106 = 0x0107, then AL &= 0x0f -> AX=0x0107; AL=0x07, AH=0x01, CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x37;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0001);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x07);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aas_aux_carry_triggers_adjust() {
    // aas: AL=0x08 (low nibble <= 9, >= 6 so no extra AH borrow), AF=1 -> the adjust
    // fires on AF alone. AX=0x0208 - 6 = 0x0202, AH-1 -> 0x0102, AL &= 0x0f -> AX=0x0102;
    // AL=0x02, AH=0x01, CF=1, AF=1.
    let mut memory = vec![0; 64];
    memory[0] = 0x3f;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0208);
    cpu.set_flag(FLAG_AF, true);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x02);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x01);
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_AF));
}

#[test]
fn aam_splits_al_into_ah_and_al() {
    // aam (0xd4 0x0a): AL=0x4B (75) -> AH=7, AL=5. SF=0, ZF=0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xd4, 0x0a]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x004b);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x05);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x07);
    assert!(!cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn aam_zero_divisor_is_divide_error() {
    // aam (0xd4 0x00): divide by zero -> #DE, delivered through the real-mode IVT.
    const ORIGIN: usize = 0x10;
    let (mut cpu, mut memory) = real_mode_cpu(&[], 0x1_0000);
    memory[ORIGIN..ORIGIN + 2].copy_from_slice(&[0xd4, 0x00]);
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x2000);
    cpu.write_reg16(Reg16::Ax, 0x004b);
    let mut bus = TestBus::with_memory(memory);

    expect_de_delivered(&mut cpu, &mut bus);
}

#[test]
fn aad_folds_ah_into_al() {
    // aad (0xd5 0x0a): AX=0x0507 (AH=5, AL=7) -> AL = 7 + 5*10 = 57 = 0x39, AH=0.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0xd5, 0x0a]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0507);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x39);
    assert_eq!(cpu.read_reg16(Reg16::Ax) >> 8, 0x00);
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn lock_add_to_memory_executes() {
    // lock add [0x40], ax (0xf0 0x01 0x06 0x40 0x00). mem[0x40]=0x0010, ax=0x0005 -> 0x0015.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0xf0, 0x01, 0x06, 0x40, 0x00]);
    memory[0x40] = 0x10;
    memory[0x41] = 0x00;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x0015
    );
}

#[test]
fn lock_bts_to_memory_executes() {
    // lock bts [0x40], ax (0xf0 0x0f 0xab 0x06 0x40 0x00). ax=3 -> set bit 3 of [0x40].
    let mut memory = vec![0; 128];
    memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xab, 0x06, 0x40, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0003);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
    assert!(!cpu.flag(FLAG_CF)); // old bit was 0
}

#[test]
fn lock_on_register_destination_delivers_ud() {
    // lock add ax, bx (0xf0 0x01 0xd8, mod=3 register dest). LOCK needs memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x01, 0xd8]);
    memory[0x18] = 0xee; // IVT[6] -> IP 0x00ee, CS 0
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_xchg_register_delivers_ud() {
    // lock xchg ax, bx (0xf0 0x87 0xd8, mod=3). XCHG needs memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x87, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_inc_register_delivers_ud() {
    // lock inc al (0xf0 0xfe 0xc0, FE /0 mod=3). INC of a register under LOCK -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0xfe, 0xc0]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_cmp_memory_delivers_ud() {
    // lock cmp [0x40], ax (0xf0 0x39 0x06 0x40 0x00). CMP is not lockable even to memory -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0xf0, 0x39, 0x06, 0x40, 0x00]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_non_lockable_opcode_delivers_ud() {
    // lock mov ax, bx (0xf0 0x89 0xd8). MOV is not lockable -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..3].copy_from_slice(&[0xf0, 0x89, 0xd8]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn lock_bts_imm_to_memory_executes() {
    // lock bts [0x40], 3 (0xf0 0x0f 0xba 0x2e 0x40 0x00 0x03, /5 = BTS). set bit 3 of [0x40].
    let mut memory = vec![0; 128];
    memory[0..7].copy_from_slice(&[0xf0, 0x0f, 0xba, 0x2e, 0x40, 0x00, 0x03]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(bus.memory[0x40], 0x08); // bit 3 set
    assert!(!cpu.flag(FLAG_CF)); // old bit was 0
}

#[test]
fn lock_btc_imm_register_delivers_ud() {
    // lock btc bx, 5 (0xf0 0x0f 0xba 0xfb 0x05, /7 = BTC, mod=3 register dest) -> #UD.
    let mut memory = vec![0; 1024];
    memory[0..5].copy_from_slice(&[0xf0, 0x0f, 0xba, 0xfb, 0x05]);
    memory[0x18] = 0xee;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00ee);
    assert!(!cpu.flag(FLAG_IF));
}

#[test]
fn bswap_reverses_dword_byte_order() {
    // bswap eax (0x0f 0xc8). eax = 0x12345678 -> 0x78563412 in 32-bit operand mode.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x0f, 0xc8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    // A 32-bit code segment so the default operand size is dword.
    let mut cs = cpu.registers.cs();
    cs.default_size_32 = true;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    cpu.write_gpr32(0, 0x1234_5678);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr32(0), 0x7856_3412);
    // A second BSWAP restores the original (round-trip).
    cpu.registers.eip = 0;
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_gpr32(0), 0x1234_5678);
}

#[test]
fn invd_and_wbinvd_noop_at_cpl0() {
    // invd (0x0f 0x08) then wbinvd (0x0f 0x09) in real mode (CPL 0). Both are no-ops:
    // they advance past their two bytes and touch no register or flag.
    let mut memory = vec![0; 64];
    memory[0..4].copy_from_slice(&[0x0f, 0x08, 0x0f, 0x09]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 2);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 4);
    assert_eq!(cpu.registers.eflags, flags_before);
}

#[test]
fn invd_at_cpl3_delivers_ud() {
    // invd (0x0f 0x08) at CPL 3 in protected mode raises #UD (vector 6).
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x08, 0, 0]);

    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn wbinvd_at_cpl3_delivers_ud() {
    // wbinvd (0x0f 0x09) at CPL 3 in protected mode raises #UD (vector 6).
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x09, 0, 0]);

    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn invlpg_memory_noop_at_cpl0() {
    // invlpg [0x40] (0x0f 0x01 0x3e 0x40 0x00, /7 with a memory operand) in real mode.
    // No TLB is modeled, so it is a no-op that advances past its bytes and leaves the
    // pointed-at memory untouched.
    let mut memory = vec![0; 128];
    memory[0..5].copy_from_slice(&[0x0f, 0x01, 0x3e, 0x40, 0x00]);
    memory[0x40] = 0xaa;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 5);
    assert_eq!(bus.memory[0x40], 0xaa);
}

#[test]
fn invlpg_at_cpl3_delivers_ud() {
    // invlpg [0x40] at CPL 3 in protected mode raises #UD (vector 6).
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ds,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0x3e, 0x40, 0x00, 0, 0, 0]);

    // INVLPG (0F 01 /7) is converted to the decode/execute split (task A12); run it through the
    // split, where the CPL-3 #UD is raised in `execute_system_seg_decoded`.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn invlpg_register_form_delivers_ud() {
    // 0F 01 /7 with a register operand (mod=3) is #UD. ModRM 0xff = mod 3, reg 7, rm 7.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0x0f, 0x01, 0xff, 0, 0]);

    // INVLPG (0F 01 /7) is converted to the decode/execute split (task A12); the register-form
    // (mod=3) #UD is raised in `execute_system_seg_decoded`.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 6,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn hardware_irq_injects_when_if_enabled() {
    // IVT[8] (physical 0x20) -> IP 0x00cc, CS 0. With IF=1 and a pending IRQ,
    // cycle() vectors to the handler before the NOP at eip 0 can execute.
    let mut memory = vec![0; 1024];
    memory[0] = 0x90; // nop that must NOT run
    memory[0x20] = 0xcc; // handler IP low byte
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    bus.pending_irq = Some(8);

    let outcome = cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x00cc);
    assert!(!cpu.flag(FLAG_IF)); // delivery clears IF
    assert!(!outcome.halted);
    assert_eq!(bus.acknowledge_interrupt(), None); // the request was consumed
}

#[test]
fn hardware_irq_held_off_when_if_clear() {
    // IF=0: the pending IRQ waits and the NOP at eip 0 runs instead.
    let mut memory = vec![0; 1024];
    memory[0] = 0x90; // nop
    memory[0x20] = 0xcc;
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);
    bus.pending_irq = Some(8);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 1); // NOP executed, no vector taken
    assert_eq!(bus.acknowledge_interrupt(), Some(8)); // still pending
}

#[test]
fn hlt_wakes_on_pending_irq() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xf4; // hlt
    memory[0x20] = 0xcc; // IVT[8] IP
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);

    // First cycle executes HLT and halts.
    assert!(cpu.cycle(&mut bus).unwrap().halted);

    // A pending IRQ wakes the CPU and is delivered on the next cycle.
    bus.pending_irq = Some(8);
    let woken = cpu.cycle(&mut bus).unwrap();
    assert!(!woken.halted);
    assert_eq!(cpu.registers.eip, 0x00cc);
}

#[test]
fn hlt_stays_halted_without_deliverable_irq() {
    let mut memory = vec![0; 1024];
    memory[0] = 0xf4; // hlt
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // execute HLT

    // No pending IRQ: stays halted.
    assert!(cpu.cycle(&mut bus).unwrap().halted);

    // Pending IRQ but IF=0: masked at the CPU, stays halted.
    cpu.set_flag(FLAG_IF, false);
    bus.pending_irq = Some(8);
    assert!(cpu.cycle(&mut bus).unwrap().halted);
}

#[test]
fn hlt_at_cpl0_protected_mode_halts() {
    // HLT is privileged (CPL 0 only), but ring 0 in protected mode is exactly
    // as permitted as real mode: require_cpl0 must not fault here.
    let mut memory = vec![0u8; 256];
    memory[0] = 0xf4; // hlt
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008, // RPL 0
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    assert!(cpu.cycle(&mut bus).unwrap().halted);
}

#[test]
fn hlt_at_cpl3_protected_mode_is_general_protection() {
    // Outside V86, a ring-3 HLT is the ordinary CPL check: #GP(0), same shape
    // as WBINVD/SYSRET's existing CPL3 tests.
    let (mut cpu, mut bus) = cpl3_code(&[0xf4]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn v86_guest_hlt_is_general_protection() {
    // A V86 task is always CPL 3 (current_privilege_level), so the guest's own
    // HLT now traps to the monitor instead of halting the machine directly
    // (the companion behavior tokaemm.asm's `.hlt` handler emulates).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "a V86 guest's HLT must land in the ring-0 monitor, not halt directly"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

#[test]
fn v86_guest_hlt_resumes_after_the_f4_byte_under_monitor_emulation() {
    // A monitor that emulates the trapped HLT (tokaemm.asm's `.hlt`: advance
    // past the F4 byte, then IRET back to V86) must land the guest one byte
    // past its HLT, still running, rather than leaving it stuck re-faulting on
    // the same instruction.
    let guest = [0xf4, 0x90]; // hlt ; nop
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &[0x00]);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    cpu.cycle(&mut bus).unwrap(); // guest HLT traps into the monitor
    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.cs().selector, R0_CS);

    // Emulate tokaemm.asm's `.hlt`: skip the error code, bump the frame's V86
    // EIP past the single-byte F4, then IRET back to V86 (mirrors the trap
    // round-trip in v86_monitor_round_trip_go_no_go).
    let esp = cpu.registers.esp() + 4;
    cpu.registers.set_esp(esp);
    let guest_eip = u32::from_le_bytes(cpu_mem(&bus, esp));
    assert_eq!(guest_eip, 0, "faulted at the guest's HLT");
    bus.memory[esp as usize..esp as usize + 4].copy_from_slice(&(guest_eip + 1).to_le_bytes());
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "IRET must return the guest to V86");
    assert_eq!(cpu.registers.eip, 1, "guest resumes past the HLT byte");
}

// --- 486 read-modify-write opcodes: XADD and CMPXCHG ---

#[test]
fn xadd_byte_swaps_and_adds_with_add_flags() {
    // 0F C0 /r XADD r/m8, r8. ModRM C3: mode 3, reg = AL(0), rm = BL(3).
    // dest = BL, src = AL. After: BL = BL + AL, AL = old BL, flags like ADD(BL, AL).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xc0, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x01); // AL (src)
    cpu.write_gpr8(3, 0xff); // BL (dest)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr8(3), 0x00); // dest = 0xff + 0x01
    assert_eq!(cpu.read_gpr8(0), 0xff); // src = old dest
    // 0xff + 0x01 wraps to 0 with carry, half-carry, and a zero result.
    assert!(cpu.flag(FLAG_CF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn xadd_word_matches_add_flags() {
    // 0F C1 /r XADD r/m16, r16. ModRM C3: reg = AX(0), rm = BX(3).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xc1, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x7fff); // AX (src)
    cpu.write_gpr16(3, 0x0001); // BX (dest)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.read_gpr16(3), 0x8000); // 0x0001 + 0x7fff
    assert_eq!(cpu.read_gpr16(0), 0x0001); // old dest
    // Signed overflow: positive + positive crossed into the sign bit.
    assert!(cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_SF));
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_ZF));
}

#[test]
fn xadd_dword_matches_add_flags() {
    // 66h is not needed: with a 32-bit operand prefix on a real-mode CS, 66 0F C1 /r.
    // ModRM C3: reg = EAX(0), rm = EBX(3).
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xc1, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x1111_1111); // src
    cpu.registers.set_ebx(0x2222_2222); // dest
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.ebx(), 0x3333_3333);
    assert_eq!(cpu.registers.eax(), 0x2222_2222);
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(!cpu.flag(FLAG_ZF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn cmpxchg_byte_equal_stores_source() {
    // 0F B0 /r CMPXCHG r/m8, r8. ModRM C3: reg = CL(1, src), rm = BL(3, dest).
    // AL == BL so ZF is set and the source (CL) is stored into BL; AL is unchanged.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x42); // AL (accumulator)
    cpu.write_gpr8(3, 0x42); // BL (dest), equal to AL
    cpu.write_gpr8(1, 0x99); // CL (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF)); // equal compare
    assert_eq!(cpu.read_gpr8(3), 0x99); // dest = src
    assert_eq!(cpu.read_gpr8(0), 0x42); // accumulator unchanged
}

#[test]
fn cmpxchg_byte_unequal_loads_destination() {
    // AL != BL: ZF clear, AL = BL, BL unchanged.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb0, 0xcb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr8(0, 0x42); // AL
    cpu.write_gpr8(3, 0x10); // BL (dest), not equal
    cpu.write_gpr8(1, 0x99); // CL (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF)); // unequal compare
    assert_eq!(cpu.read_gpr8(0), 0x10); // accumulator = dest
    assert_eq!(cpu.read_gpr8(3), 0x10); // dest unchanged
    // Flags must match CMP(0x42, 0x10) = 0x32: no borrow, positive, nonzero.
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn cmpxchg_word_equal_stores_source() {
    // 0F B1 /r CMPXCHG r/m16, r16. ModRM C3: reg = CX(1, src), rm = BX(3, dest).
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0x0f, 0xb1, 0xcb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x1234); // AX
    cpu.write_gpr16(3, 0x1234); // BX, equal
    cpu.write_gpr16(1, 0xbeef); // CX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_gpr16(3), 0xbeef);
    assert_eq!(cpu.read_gpr16(0), 0x1234);
}

#[test]
fn cmpxchg_dword_unequal_loads_destination() {
    // 66 0F B1 /r CMPXCHG r/m32, r32. ModRM C3: reg = ECX(1, src), rm = EBX(3, dest).
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0x66, 0x0f, 0xb1, 0xcb]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0xaaaa_aaaa); // EAX
    cpu.registers.set_ebx(0x5555_5555); // EBX (dest), not equal
    cpu.registers.set_ecx(0xdead_beef); // ECX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.eax(), 0x5555_5555); // accumulator = dest
    assert_eq!(cpu.registers.ebx(), 0x5555_5555); // dest unchanged
}

#[test]
fn lock_xadd_to_memory_is_accepted() {
    // F0 0F C1 06 00 02: LOCK XADD [0x0200], AX. ModRM 06 is mode 0 rm 6 (direct disp16),
    // a memory destination, so the LOCK is legal and the instruction runs.
    let mut memory = vec![0; 1024];
    memory[0..6].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0x06, 0x00, 0x02]);
    memory[0x200..0x202].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_gpr16(0, 0x0001); // AX (src)
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    // [0x0200] = 0x0010 + 0x0001, AX = old [0x0200].
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x200], bus.memory[0x201]]),
        0x0011
    );
    assert_eq!(cpu.read_gpr16(0), 0x0010);
}

#[test]
fn lock_xadd_to_register_is_undefined_opcode() {
    // F0 0F C1 C3: LOCK XADD BX, AX. The register destination makes the LOCK prefix illegal,
    // so the decoder raises #UD (vector 6) before executing.
    let mut memory = vec![0; 16];
    memory[0..4].copy_from_slice(&[0xf0, 0x0f, 0xc1, 0xc3]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn lock_bswap_is_undefined_opcode() {
    // F0 0F C8: LOCK BSWAP EAX. BSWAP has no memory form, so LOCK is always #UD.
    let mut memory = vec![0; 16];
    memory[0..3].copy_from_slice(&[0xf0, 0x0f, 0xc8]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

// Build a CPL-3 protected-mode CPU whose CS and DS are flat user segments, running
// MOV AX, moffs16 (0xa1) that reads a word from DS:moffs. The caller picks the
// moffs so the access lands on an even or odd boundary.
fn cpl3_word_read_at(moffs: u16) -> (Cpu386, TestBus) {
    let mut memory = vec![0; 256];
    memory[0] = 0xa1;
    memory[1..3].copy_from_slice(&moffs.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ds,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    (cpu, TestBus::with_memory(memory))
}

#[test]
fn misaligned_word_read_faults_ac_when_am_and_ac_set_at_cpl3() {
    // CR0.AM and EFLAGS.AC both set, CPL 3, odd word address: #AC (vector 17, no
    // error code).
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);

    // 0xa1 (MOV AX, moffs) is converted to the split, so drive it through the split executor;
    // the legacy fused entry no longer carries that arm. The #AC alignment check fires in the
    // shared memory-read helper either way.
    let result = exec_one_split(&mut cpu, &mut bus);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 17,
                error_code: None
            })
        ),
        "{result:?}"
    );
}

#[test]
fn misaligned_word_read_no_fault_without_cr0_am() {
    // EFLAGS.AC set but CR0.AM clear: the alignment check stays masked, no fault.
    // Set CR0 bit 4 (ET) too: it is not AM, so it must not arm the check.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= 0x0000_0010; // bit 4 (ET), not AM
    cpu.set_flag(FLAG_AC, true);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn misaligned_word_read_no_fault_without_eflags_ac() {
    // CR0.AM set but EFLAGS.AC clear: software has not opted in, no fault.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn misaligned_word_read_no_fault_at_supervisor() {
    // AM and AC both set, but CPL 0 (supervisor): exempt, no fault. Reuse the
    // CPL-3 setup and drop CS/DS RPL to 0.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0041);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);
    let mut cs = cpu.registers.cs();
    cs.selector = 0x0000;
    cpu.registers.set_segment(SegmentIndex::Cs, cs);
    let mut ds = cpu.registers.segment(SegmentIndex::Ds);
    ds.selector = 0x0000;
    cpu.registers.set_segment(SegmentIndex::Ds, ds);
    cpu.cpl = 0; // dropped CS's RPL to 0 above; seed the cached CPL to match

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn aligned_word_read_never_faults_with_am_and_ac() {
    // Even word address: aligned, so no #AC even with AM and AC set at CPL 3.
    let (mut cpu, mut bus) = cpl3_word_read_at(0x0040);
    cpu.control.cr0 |= CR0_AM;
    cpu.set_flag(FLAG_AC, true);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
}

#[test]
fn eflags_ac_and_id_survive_pushf_popf_round_trip() {
    // 66 9c PUSHFD ; 66 9d POPFD. Set AC and ID, perturb both after they reach the
    // stack, and confirm POPFD restores them from the dword flag image.
    let mut memory = vec![0; 1024];
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    memory[2..4].copy_from_slice(&[0x66, 0x9d]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.set_flag(FLAG_AC, true);
    cpu.set_flag(FLAG_ID, true);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // pushfd
    cpu.set_flag(FLAG_AC, false); // perturb after the image is on the stack
    cpu.set_flag(FLAG_ID, false);
    cpu.cycle(&mut bus).unwrap(); // popfd

    assert!(cpu.flag(FLAG_AC));
    assert!(cpu.flag(FLAG_ID));
}

fn run_cpuid(leaf: u32) -> Cpu386 {
    // CPUID (0F A2) with the leaf selector in EAX. Returns the CPU after one step so the
    // caller can read EAX/EBX/ECX/EDX.
    let mut memory = vec![0; 64];
    memory[0..2].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(leaf);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu
}

#[test]
fn cpuid_leaf0_reports_vendor_string_and_max_leaf() {
    let cpu = run_cpuid(0);
    // Max basic leaf is 1.
    assert_eq!(cpu.registers.eax(), 1);
    // Vendor string "Genuine GSW " in the standard EBX, EDX, ECX order, four bytes
    // little-endian per register.
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
    assert_eq!(cpu.registers.edx().to_le_bytes(), *b"ine ");
    assert_eq!(cpu.registers.ecx().to_le_bytes(), *b"GSW ");
    // Concatenating EBX:EDX:ECX yields the full 12-byte vendor string.
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
    vendor[4..8].copy_from_slice(&cpu.registers.edx().to_le_bytes());
    vendor[8..12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
    assert_eq!(&vendor, b"Genuine GSW ");
}

#[test]
fn cpuid_leaf1_reports_family5_and_mmx_without_fpu() {
    let cpu = run_cpuid(1);
    let eax = cpu.registers.eax();
    // Family is bits 11-8 and must be 5 (586 / K6 class).
    assert_eq!((eax >> 8) & 0xf, 5);
    // Type is bits 13-12 (OEM = 0).
    assert_eq!((eax >> 12) & 0x3, 0);
    // MMX is bit 23 of EDX; FPU is bit 0 and must be off.
    let edx = cpu.registers.edx();
    assert_ne!(edx & (1 << 23), 0, "MMX bit should be set");
    assert_eq!(edx & 1, 0, "FPU bit should be clear");
    // Brand index 0 and no extended feature claimed.
    assert_eq!(cpu.registers.ebx(), 0);
    assert_eq!(cpu.registers.ecx(), 0);
}

#[test]
fn cpuid_unknown_leaf_returns_zeros() {
    let cpu = run_cpuid(0x4000_0000);
    assert_eq!(cpu.registers.eax(), 0);
    assert_eq!(cpu.registers.ebx(), 0);
    assert_eq!(cpu.registers.ecx(), 0);
    assert_eq!(cpu.registers.edx(), 0);
}

#[test]
fn cpuid_brand_string_reports_genuine_gsw_80586() {
    // Leaf 0x80000000 reports the maximum extended leaf, and 0x80000002..0x80000004
    // return the 48-byte null-padded brand string, 16 bytes per leaf in EAX, EBX, ECX,
    // EDX order. Concatenated, they spell the full processor name "Genuine GSW-80586".
    assert_eq!(run_cpuid(0x8000_0000).registers.eax(), 0x8000_0006);
    let mut brand = [0u8; 48];
    for (i, leaf) in [0x8000_0002u32, 0x8000_0003, 0x8000_0004]
        .iter()
        .enumerate()
    {
        let cpu = run_cpuid(*leaf);
        let base = i * 16;
        brand[base..base + 4].copy_from_slice(&cpu.registers.eax().to_le_bytes());
        brand[base + 4..base + 8].copy_from_slice(&cpu.registers.ebx().to_le_bytes());
        brand[base + 8..base + 12].copy_from_slice(&cpu.registers.ecx().to_le_bytes());
        brand[base + 12..base + 16].copy_from_slice(&cpu.registers.edx().to_le_bytes());
    }
    let mut expected = [0u8; 48];
    expected[..17].copy_from_slice(b"Genuine GSW-80586");
    assert_eq!(brand, expected);
}

#[test]
fn cpuid_is_not_privileged_at_cpl3() {
    // CPUID runs at any privilege level. In protected mode at CPL 3 it must execute,
    // not fault, and still report the GSW-586 identity.
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false,
        },
    );
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(vec![0x0f, 0xa2, 0, 0]);

    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.registers.eax(), 1);
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
}

#[test]
fn default_level_is_full_isa() {
    // The core resets to the full ISA so firmware POST is never restricted.
    assert_eq!(Cpu386::default().level(), CpuLevel::I586);
}

#[test]
fn cpu_level_cache_table() {
    assert_eq!(CpuLevel::I286.cache_kb(), (0, 0));
    assert_eq!(CpuLevel::I386.cache_kb(), (0, 64));
    assert_eq!(CpuLevel::I486.cache_kb(), (16, 128));
    assert_eq!(CpuLevel::I586.cache_kb(), (32, 512));
}

// --- Phase 5 Slice A: RDTSC, RDMSR/WRMSR, the K6 MSR set, CR4 ---

fn cpl3_code(code: &[u8]) -> (Cpu386, TestBus) {
    // Protected mode with a flat CPL-3 code segment (selector RPL 3), the same shape
    // the #AC/CPUID privilege tests use, but loaded with arbitrary code.
    let mut memory = vec![0u8; 256];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    // This helper builds CPL-3 state directly (no transfer instruction runs), so the
    // cached `cpl` must be seeded to match the CS RPL it just installed by hand.
    cpu.cpl = 3;
    (cpu, TestBus::with_memory(memory))
}

#[test]
fn level_timing_scales_instruction_clocks_per_mode() {
    // 01 D8 is ADD AX, BX: a register ALU op that never faults. This measures the
    // CPU's INSTRUCTION-clock charge only (cpu.elapsed_clocks holds scaled core
    // clocks; bus/fetch clocks are accounted on the bus, not here).
    //
    // Calibration note (B-T10): the per-mode `level_timing` scalar is the COMPUTE
    // dial only. The per-mode BUS scalar (`bus_timing`, applied in the machine's
    // `scale_bus`) now carries the modes' absolute benchmark magnitude, so a fast
    // mode pulls ahead via the bus, NOT by charging fewer instruction clocks. The
    // compute dial just trims each mode's compute share to seat Dhrystone: it is
    // largest on the 286 (most compute-heavy in-order ratio), smaller on the 386,
    // and smallest-and-EQUAL on the 486 and 586 (their pull-ahead is all in the
    // bus dial). The contract this test guards: the scalar is applied per level,
    // descends 286 > 386 >= 486, and the 586 charges no more than the 486 (the bus
    // dial, not this one, carries the 586's speed). A mode change re-scales.
    fn elapsed_for(level: CpuLevel) -> u64 {
        let (mut cpu, memory) = real_mode_cpu(&[0x01, 0xd8], 0x20);
        cpu.set_level(level);
        let mut bus = TestBus::with_memory(memory);
        for _ in 0..1000 {
            cpu.registers.eip = 0;
            cpu.cycle(&mut bus).unwrap();
        }
        cpu.elapsed_clocks
    }
    let i286 = elapsed_for(CpuLevel::I286);
    let i386 = elapsed_for(CpuLevel::I386);
    let i486 = elapsed_for(CpuLevel::I486);
    let i586 = elapsed_for(CpuLevel::I586);
    // 286 (3/5) charges more instruction clocks than the 386 (2/5).
    assert!(
        i286 > i386,
        "286 ({i286}) should charge more instruction clocks than 386 ({i386})"
    );
    // 386 (2/5) charges more than the small-and-equal 486/586 (1/12).
    assert!(
        i386 > i486,
        "386 ({i386}) should charge more instruction clocks than 486 ({i486})"
    );
    // 486 and 586 share the same compute ratio (1/12): the bus dial, not this
    // one, carries the 586's pull-ahead, so the 586 charges no MORE than the 486.
    assert!(
        i586 <= i486,
        "586 ({i586}) shares the 486's compute ratio and must charge no more than 486 ({i486})"
    );
}

#[test]
fn rdtsc_reads_elapsed_core_clocks_into_edx_eax() {
    // 0F 31. EDX:EAX take the 64-bit time-stamp counter (the running core-clock count).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x31], 0x20);
    cpu.elapsed_clocks = 0x1_0000_0002;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x0000_0002);
    assert_eq!(cpu.registers.edx(), 0x0000_0001);
}

#[test]
fn wrmsr_rdmsr_round_trip_whcr() {
    // 0F 30 WRMSR then 0F 32 RDMSR with ECX selecting WHCR. EDX:EAX is the 64-bit value.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x32], 0x20);
    cpu.registers.set_ecx(MSR_WHCR);
    cpu.registers.set_edx(0xdead_beef);
    cpu.registers.set_eax(0x0bad_f00d);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // wrmsr
    assert_eq!(cpu.msr.whcr, 0xdead_beef_0bad_f00d);
    cpu.registers.set_eax(0);
    cpu.registers.set_edx(0);
    cpu.cycle(&mut bus).unwrap(); // rdmsr
    assert_eq!(cpu.registers.edx(), 0xdead_beef);
    assert_eq!(cpu.registers.eax(), 0x0bad_f00d);
}

#[test]
fn wrmsr_efer_rejects_reserved_bits() {
    // Only EFER.SCE (bit 0) is writable; any reserved bit set raises #GP(0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_EFER);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(0x2); // bit 1 is reserved
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn wrmsr_star_rejects_reserved_bits() {
    // STAR bits 63-48 are reserved; setting one raises #GP(0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_STAR);
    cpu.registers.set_edx(0x0001_0000); // bit 48 set
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn wrmsr_efer_and_star_accept_their_defined_bits() {
    // SCE in EFER and the selector base / target EIP in STAR write without faulting.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30, 0x0f, 0x30], 0x20);
    cpu.registers.set_ecx(MSR_EFER);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(EFER_SCE as u32);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.msr.efer, EFER_SCE);
    cpu.registers.set_ecx(MSR_STAR);
    cpu.registers.set_edx(0x0000_ffff); // selector base in 47-32
    cpu.registers.set_eax(0x0001_0000); // target EIP in 31-0
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.msr.star, 0x0000_ffff_0001_0000);
}

#[test]
fn wrmsr_tsc_rebases_so_the_counter_reads_the_written_value() {
    // Writing the TSC stores an offset such that the running core-clock count reads
    // back as the written value. execute_instruction does not advance elapsed_clocks,
    // so the read is exact.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x30], 0x20);
    cpu.elapsed_clocks = 500;
    cpu.registers.set_ecx(MSR_TSC);
    cpu.registers.set_edx(0);
    cpu.registers.set_eax(1_000_000);
    let mut bus = TestBus::with_memory(memory);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.time_stamp_counter(), 1_000_000);
}

#[test]
fn wrmsr_is_general_protection_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x30]);
    cpu.registers.set_ecx(MSR_WHCR);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdmsr_unknown_selector_is_general_protection() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x32], 0x20);
    cpu.registers.set_ecx(0x1234_5678);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdtsc_is_general_protection_when_tsd_set_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
    cpu.control.cr4 |= CR4_TSD;
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn rdtsc_runs_at_cpl3_when_tsd_clear() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x31]);
    cpu.elapsed_clocks = 42;
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.registers.eax(), 42);
}

#[test]
fn mov_cr4_round_trips() {
    // 0F 22 E0 = MOV CR4, EAX (reg=4, rm=EAX); 0F 20 E3 = MOV EBX, CR4 (reg=4, rm=EBX).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0, 0x0f, 0x20, 0xe3], 0x20);
    cpu.registers.set_eax(CR4_TSD);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr4, CR4_TSD);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), CR4_TSD);
}

#[test]
fn mov_cr_write_faults_at_cpl3() {
    // 0F 22 C0 = MOV CR0, EAX (reg=0, rm=EAX). A ring-3 write to CR0 must
    // never silently succeed -- it is a privileged instruction like every
    // other 0F 00/01 system-register op (LLDT/LTR/LMSW/CLTS all gate on
    // require_cpl0). Mirrors the cpl3_code + vector-13 shape used by the
    // RDMSR/RDTSC privilege tests above.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x22, 0xc0]);
    cpu.registers.set_eax(CR0_PE);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_cr_read_faults_at_cpl3() {
    // 0F 20 C0 = MOV EAX, CR0 (reg=0, rm=EAX). The read side has the same
    // gap as the write side; a ring-3 guest must not be able to probe CR0
    // either.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x20, 0xc0]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

// --- Batch F: LGDT/LIDT privilege, MOV CR0 PG/PE, undefined CR#, CR4 mask, CR3 PWT/PCD ---

#[test]
fn lgdt_faults_at_cpl3() {
    // 0F 01 16 xx xx = LGDT [disp16]. 386 PRM 5.1: LGDT is privileged like every other
    // 0F 00/01 system-register op, so a ring-3 guest must get #GP(0), not a silent
    // table reload.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x01, 0x16, 0x40, 0x00]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn lidt_faults_at_cpl3() {
    // 0F 01 1E xx xx = LIDT [disp16]. Same gate as LGDT above.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x01, 0x1e, 0x40, 0x00]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn lgdt_lidt_run_at_cpl0_in_real_mode() {
    // Real mode has no protection, so CPL is always 0 there; the new require_cpl0 gate
    // on LGDT/LIDT must not regress the existing 286-boot-code path (mirrors
    // lgdt_still_runs_at_286 above, but exercised here as the CPL0-unaffected half of
    // the row-22 contract).
    let mut memory = vec![0; 1024];
    // LGDT [0x0020] (5 bytes: opcode+modrm+disp16); LIDT [0x0026] starts right after.
    memory[0..5].copy_from_slice(&[0x0f, 0x01, 0x16, 0x20, 0x00]);
    memory[5..10].copy_from_slice(&[0x0f, 0x01, 0x1e, 0x26, 0x00]);
    memory[0x20..0x26].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    memory[0x26..0x2c].copy_from_slice(&[0xff, 0x01, 0x00, 0x20, 0x00, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.gdtr.base, 0x0000_1000);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.idtr.base, 0x0000_2000);
}

#[test]
fn mov_cr0_setting_pg_without_pe_is_general_protection() {
    // 0F 22 C0 = MOV CR0, EAX. 386 PRM 5.2.1: PG (bit 31) with PE (bit 0) clear is an
    // invalid combination -- paging requires protection. Run at CPL 0 (real mode) so
    // the fault is specifically the PG/PE check, not the row-23 privilege gate.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xc0], 0x20);
    cpu.registers.set_eax(CR0_PG);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
    // The rejected write must not have taken effect.
    assert_eq!(cpu.control.cr0 & CR0_PG, 0);
}

#[test]
fn mov_cr0_setting_pg_with_pe_succeeds() {
    // The companion case: PG with PE both set in the same write is the normal way
    // protected-mode paging turns on and must not fault.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xc0], 0x20);
    cpu.registers.set_eax(CR0_PE | CR0_PG);
    let mut bus = TestBus::with_memory(memory);
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.control.cr0 & (CR0_PE | CR0_PG), CR0_PE | CR0_PG);
}

#[test]
fn mov_from_undefined_cr_is_undefined_opcode() {
    // 0F 20 C8 = MOV EAX, CR1 (reg=1, rm=EAX). CR1/CR5/CR6/CR7 have no backing
    // register on the 386/486/586 architecture; referencing one is #UD, not a
    // silent read of 0.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x20, 0xc8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_to_undefined_cr_is_undefined_opcode() {
    // 0F 22 F8 = MOV CR7, EAX (reg=7, rm=EAX). Same undefined-register contract as
    // the read side, checked on the write path.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xf8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_cr4_accepts_defined_bits() {
    // 0F 22 E0 = MOV CR4, EAX; 0F 20 E3 = MOV EBX, CR4. Only the bits this GSW-586
    // persona defines (VME/PVI/TSD/DE/PSE/MCE/GPE, CR4_DEFINED_MASK) exist; writing
    // exactly that set round-trips.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0, 0x0f, 0x20, 0xe3], 0x20);
    cpu.registers.set_eax(CR4_DEFINED_MASK);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr4, CR4_DEFINED_MASK);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), CR4_DEFINED_MASK);
}

#[test]
fn mov_cr4_rejects_reserved_bits() {
    // 0F 22 E0 = MOV CR4, EAX. The AMD-K6 guide's MOV-to/from-CR4 exception table and
    // the Pentium Vol. 3 instruction reference both fault ("#GP(0)"/"Interrupt 13")
    // if a 1 is written to any reserved bit -- including one way outside the defined
    // byte, like bit 31 -- in every mode, not just protected mode. CR4 is left
    // unmodified (the same convention as CR0's PG/PE gate above).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xe0], 0x20);
    cpu.registers.set_eax(0xffff_ffff);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
    assert_eq!(cpu.control.cr4, 0);
}

// --- Ledger row 25: MOV to/from debug registers (0F 21/0F 23) ---

#[test]
fn mov_dr7_round_trips() {
    // 0F 23 F8 = MOV DR7, EAX (reg=7, rm=EAX); 0F 21 FB = MOV EBX, DR7 (reg=7, rm=EBX).
    // Bit 10 is hardwired to 1 (DR7_FIXED_ONE) per 386 PRM ch12, so it must read back
    // set even though the write below does not include it.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xf8, 0x0f, 0x21, 0xfb], 0x20);
    cpu.registers.set_eax(0x0000_0155); // L0/G0/L1/G1/L2 enables, bit 10 not set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr7, 0x0000_0155 | DR7_FIXED_ONE);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_0155 | DR7_FIXED_ONE);
}

#[test]
fn mov_dr6_round_trips_with_reserved_bit_behavior() {
    // 0F 23 F0 = MOV DR6, EAX (reg=6, rm=EAX); 0F 21 F3 = MOV EBX, DR6 (reg=6, rm=EBX).
    // DR6 is plain storage here (breakpoint matching is ledger row 26, deferred), so
    // whatever is written reads back byte-for-byte, including into the high bits the
    // PRM defines as fixed-1 on reset -- this core does not re-force them on write.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xf0, 0x0f, 0x21, 0xf3], 0x20);
    cpu.registers.set_eax(0x0000_000f); // B0-B3 all set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr6, 0x0000_000f);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_000f);
}

#[test]
fn mov_dr6_reset_value_matches_prm() {
    // 386 PRM ch12: DR6 powers up as 0xFFFF_0FF0.
    let cpu = Cpu386::default();
    assert_eq!(cpu.control.dr6, 0xffff_0ff0);
}

#[test]
fn mov_dr7_reset_value_matches_prm() {
    // 386 PRM ch12: DR7 powers up as 0x0000_0400 (bit 10 set, everything else clear).
    let cpu = Cpu386::default();
    assert_eq!(cpu.control.dr7, 0x0000_0400);
}

#[test]
fn mov_dr4_aliases_dr6() {
    // 0F 23 E0 = MOV DR4, EAX (reg=4); 0F 21 E3 = MOV EBX, DR4. With CR4.DE clear (the
    // default -- never behaviorally set by this core), DR4 aliases DR6.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xe0, 0x0f, 0x21, 0xe3], 0x20);
    cpu.registers.set_eax(0x0000_000a);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr6, 0x0000_000a);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_000a);
}

#[test]
fn mov_dr5_aliases_dr7() {
    // 0F 23 E8 = MOV DR5, EAX (reg=5); 0F 21 EB = MOV EBX, DR5. Aliases DR7, same as
    // DR4 aliases DR6 above.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xe8, 0x0f, 0x21, 0xeb], 0x20);
    cpu.registers.set_eax(0x0000_0001);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr7, 0x0000_0001 | DR7_FIXED_ONE);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0000_0001 | DR7_FIXED_ONE);
}

#[test]
fn mov_dr0_3_round_trip() {
    // 0F 23 D8 = MOV DR3, EAX (reg=3); 0F 21 DB = MOV EBX, DR3. Linear breakpoint
    // address storage only, no matching implemented.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x23, 0xd8, 0x0f, 0x21, 0xdb], 0x20);
    cpu.registers.set_eax(0xdead_beef);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.dr0_3[3], 0xdead_beef);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0xdead_beef);
}

#[test]
fn mov_dr_write_faults_at_cpl3() {
    // 0F 23 F8 = MOV DR7, EAX. Debug-register access is privileged (386 PRM ch12):
    // a ring-3 guest must get #GP(0), not a silent write.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x23, 0xf8]);
    cpu.registers.set_eax(0);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_dr_read_faults_at_cpl3() {
    // 0F 21 F8 = MOV EAX, DR7. Same gate on the read side.
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x21, 0xf8]);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn mov_dr_memory_operand_is_undefined_opcode() {
    // 0F 21 00 = MOV [BX+SI], DR0 with mode=0 (memory operand) instead of mode=3
    // (register). Debug-register moves are register-form only; any other ModRM mode
    // is an invalid encoding (#UD), same convention as MOV CR.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x21, 0x00], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn mov_cr3_round_trips_pwt_and_pcd() {
    // 0F 22 D8 = MOV CR3, EAX (reg=3, rm=EAX); 0F 20 DB = MOV EBX, CR3 (reg=3, rm=EBX).
    // 386 PRM 5.2.2 defines the page-directory base in bits 31:12; PWT/PCD (bits 4:3)
    // are a 486+ addition (Pentium Vol. 3 S9/S18.3) this persona implements as
    // cache-control hints. A guest that sets PWT/PCD must read them back; only bits
    // 2:0 (always reserved/0) and the base-alignment bits outside 31:12 are not
    // preserved by the base itself.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x22, 0xd8, 0x0f, 0x20, 0xdb], 0x20);
    // Base 0x00123000 with PWT (bit 3) and PCD (bit 4) both set, plus reserved bits
    // 2:0 set to confirm those stay masked off.
    cpu.registers.set_eax(0x0012_3007 | 0x18);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr3, 0x0012_3018);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ebx(), 0x0012_3018);
}

#[test]
fn cpuid_leaf1_reports_tsc_and_msr() {
    let edx = run_cpuid(1).registers.edx();
    assert_ne!(edx & (1 << 4), 0, "TSC feature bit should be set");
    assert_ne!(edx & (1 << 5), 0, "MSR feature bit should be set");
}

#[test]
fn rdtsc_is_undefined_opcode_below_586() {
    // RDTSC is a 586 addition: #UD at the throttled 486 level, fine at 586.
    let code = [0x0f, 0x31];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

// --- Phase 5 Slice B: CMOVcc, FCMOVcc, FCOMI/FUCOMI ---

#[test]
fn cmove_word_moves_low_half_when_zf_set() {
    // 0F 44 C3: CMOVE AX, BX (16-bit operand in real mode).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    // A 16-bit move writes only the low word; the upper half of EAX is preserved.
    assert_eq!(cpu.registers.eax(), 0x1111_bbbb);
}

#[test]
fn cmove_does_not_move_when_zf_clear() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x44, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0x1111_1111);
}

#[test]
fn cmovne_dword_moves_when_zf_clear() {
    // 66 0F 45 C3: CMOVNE EAX, EBX (32-bit operand).
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x0f, 0x45, 0xc3], 0x20);
    cpu.registers.set_eax(0x1111_1111);
    cpu.registers.set_ebx(0xaaaa_bbbb);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
}

#[test]
fn cmovcc_is_undefined_opcode_below_586() {
    let code = [0x0f, 0x44, 0xc3];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn fcmove_moves_st1_into_st0_when_zf_set() {
    // DA C9: FCMOVE ST(0), ST(1).
    let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
    cpu.fpu.push(2.0); // ST(1)
    cpu.fpu.push(1.0); // ST(0)
    cpu.set_flag(FLAG_ZF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 2.0);
}

#[test]
fn fcmove_leaves_st0_when_zf_clear() {
    let (mut cpu, memory) = real_mode_cpu(&[0xda, 0xc9], 0x20);
    cpu.fpu.push(2.0);
    cpu.fpu.push(1.0);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fcmovnb_moves_st1_into_st0_when_cf_clear() {
    // DB C1: FCMOVNB ST(0), ST(1).
    let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xc1], 0x20);
    cpu.fpu.push(7.0); // ST(1)
    cpu.fpu.push(3.0); // ST(0)
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 7.0);
}

#[test]
fn fcomi_sets_integer_flags_from_the_comparison() {
    // DB F1: FCOMI ST(0), ST(1). The result lands in ZF/PF/CF.
    fn run(st0: f64, st1: f64) -> (bool, bool, bool) {
        let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xf1], 0x40);
        cpu.fpu.push(st1);
        cpu.fpu.push(st0);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        (cpu.flag(FLAG_ZF), cpu.flag(FLAG_PF), cpu.flag(FLAG_CF))
    }
    assert_eq!(run(2.0, 1.0), (false, false, false)); // ST0 > ST1
    assert_eq!(run(1.0, 2.0), (false, false, true)); // ST0 < ST1
    assert_eq!(run(1.0, 1.0), (true, false, false)); // equal
    assert_eq!(run(f64::NAN, 1.0), (true, true, true)); // unordered
}

#[test]
fn fcomip_compares_then_pops() {
    // DF F1: FCOMIP ST(0), ST(1). Equal operands set ZF, then ST(0) is popped.
    let (mut cpu, memory) = real_mode_cpu(&[0xdf, 0xf1], 0x40);
    cpu.fpu.push(2.0);
    cpu.fpu.push(2.0);
    let top_before = cpu.fpu.top();
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.fpu.top(), (top_before + 1) & 7);
}

// --- Phase 5 Slice C: CMPXCHG8B ---

fn read_dword(memory: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([memory[at], memory[at + 1], memory[at + 2], memory[at + 3]])
}

#[test]
fn cmpxchg8b_equal_stores_ecx_ebx_and_sets_zf() {
    // 0F C7 0E 40 00: CMPXCHG8B [0x0040] (reg=/1, mod=0 rm=6 direct disp16).
    let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x44].copy_from_slice(&0x5566_7788u32.to_le_bytes());
    memory[0x44..0x48].copy_from_slice(&0x1122_3344u32.to_le_bytes());
    cpu.registers.set_eax(0x5566_7788); // EDX:EAX equals the memory value
    cpu.registers.set_edx(0x1122_3344);
    cpu.registers.set_ebx(0xcafe_babe); // ECX:EBX is the value to store
    cpu.registers.set_ecx(0xdead_beef);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(read_dword(&bus.memory, 0x40), 0xcafe_babe);
    assert_eq!(read_dword(&bus.memory, 0x44), 0xdead_beef);
}

#[test]
fn cmpxchg8b_unequal_loads_edx_eax_and_clears_zf() {
    let (mut cpu, mut memory) = real_mode_cpu(&[0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x44].copy_from_slice(&0xaaaa_bbbbu32.to_le_bytes());
    memory[0x44..0x48].copy_from_slice(&0xcccc_ddddu32.to_le_bytes());
    cpu.registers.set_eax(0x0000_0001); // EDX:EAX differs from memory
    cpu.registers.set_edx(0x0000_0002);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.flag(FLAG_ZF));
    assert_eq!(cpu.registers.eax(), 0xaaaa_bbbb);
    assert_eq!(cpu.registers.edx(), 0xcccc_dddd);
    assert_eq!(read_dword(&bus.memory, 0x40), 0xaaaa_bbbb); // memory unchanged
}

#[test]
fn cmpxchg8b_register_form_is_undefined_opcode() {
    // 0F C7 C9: mod=3 register form is #UD. CMPXCHG8B is converted (`DecodeGroup::Misc`), so
    // drive it through the split — the executor re-detects the register form and #UDs.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn cmpxchg8b_wrong_group_extension_is_undefined_opcode() {
    // 0F C7 06 40 00: reg=/0, not CMPXCHG8B -> #UD. Driven through the split (converted).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xc7, 0x06, 0x40, 0x00], 0x80);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn lock_cmpxchg8b_to_memory_is_accepted() {
    // F0 0F C7 0E 40 00: LOCK CMPXCHG8B [0x0040].
    let (mut cpu, mut memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0x0e, 0x40, 0x00], 0x80);
    memory[0x40..0x48].copy_from_slice(&0u64.to_le_bytes());
    cpu.registers.set_ebx(0x11);
    cpu.registers.set_ecx(0x22);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF)); // EDX:EAX = 0 equals zeroed memory
    assert_eq!(read_dword(&bus.memory, 0x40), 0x11);
    assert_eq!(read_dword(&bus.memory, 0x44), 0x22);
}

#[test]
fn lock_cmpxchg8b_register_form_is_undefined_opcode() {
    // F0 0F C7 C9: LOCK on the register form -> #UD.
    let (mut cpu, memory) = real_mode_cpu(&[0xf0, 0x0f, 0xc7, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn cpuid_leaf1_reports_cx8() {
    let edx = run_cpuid(1).registers.edx();
    assert_ne!(edx & (1 << 8), 0, "CX8 feature bit should be set");
}

#[test]
fn cmpxchg8b_is_undefined_opcode_below_586() {
    let code = [0x0f, 0xc7, 0x0e, 0x40, 0x00];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I486).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

// --- Phase 5 Slice D: SYSCALL/SYSRET and RSM ---

#[test]
fn syscall_jumps_to_star_target_and_loads_flat_segments() {
    // 0F 05 SYSCALL. STAR: target EIP = 0x0001_0000, CS/SS selector base = 0x08.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x40);
    cpu.msr.efer = EFER_SCE;
    cpu.msr.star = (0x0008u64 << 32) | 0x0001_0000;
    cpu.set_flag(FLAG_IF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.ecx(), 2); // return address (EIP past the 2-byte SYSCALL)
    assert_eq!(cpu.registers.eip, 0x0001_0000);
    let cs = cpu.registers.cs();
    assert_eq!(cs.selector, 0x08);
    assert_eq!(cs.base, 0);
    assert_eq!(cs.limit, 0xffff_ffff);
    assert!(cs.default_size_32);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x10); // base + 8
    assert!(!cpu.flag(FLAG_IF)); // SYSCALL clears IF
}

#[test]
fn syscall_is_undefined_opcode_without_sce() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x05], 0x20);
    cpu.msr.efer = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn sysret_returns_to_ecx_with_cpl3_and_sets_if() {
    // 0F 07 SYSRET. STAR CS/SS base = 0x08; SYSRET forces RPL 3 and SS = base + 16.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
    cpu.msr.efer = EFER_SCE;
    cpu.msr.star = 0x0008u64 << 32;
    cpu.registers.set_ecx(0x0002_0000);
    cpu.set_flag(FLAG_IF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0002_0000);
    assert_eq!(cpu.registers.cs().selector, 0x0b); // 0x08 | 3
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x1b); // (0x08 + 16) | 3
    assert!(cpu.flag(FLAG_IF)); // SYSRET sets IF
}

#[test]
fn sysret_is_general_protection_at_cpl3() {
    let (mut cpu, mut bus) = cpl3_code(&[0x0f, 0x07]);
    cpu.msr.efer = EFER_SCE;
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(
        fault,
        InternalFault::Exception {
            vector: 13,
            error_code: Some(0)
        }
    ));
}

#[test]
fn sysret_is_undefined_opcode_without_sce() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x07], 0x20);
    cpu.msr.efer = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn rsm_is_undefined_opcode_outside_smm() {
    // No SMM is modeled, so RSM always faults #UD.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0xaa], 0x20);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn syscall_is_undefined_opcode_below_586() {
    // Even with SCE enabled, a throttled 486-level guest sees #UD from the 586 gate.
    let mut memory = vec![0u8; 64];
    memory[..2].copy_from_slice(&[0x0f, 0x05]);
    let mut cpu = Cpu386::default();
    cpu.set_level(CpuLevel::I486);
    cpu.msr.efer = EFER_SCE;
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

/// Run one instruction through the production decode/execute split and return the raw
/// `InternalFault` (without exception delivery), so a test can assert `is_ok()`/`unwrap_err()`
/// directly on the result. This is the single per-instruction entry the test suite uses now that
/// the transitional fused reference is gone: it is exactly what `cycle` runs, minus the
/// interrupt-service prologue and the exception-delivery epilogue.
fn exec_one_split<B: CpuBus>(cpu: &mut Cpu386, bus: &mut B) -> ExecResult<CycleOutcome> {
    cpu.begin_instruction();
    let insn = cpu.decode(bus)?;
    cpu.execute_decoded(&insn, bus)
}

#[test]
fn decoded_insn_stays_dense() {
    // The decode cache stores one DecodedInsn per line. At the chosen 2048 lines this caps the
    // footprint near 96 KB (see DECODE_CACHE_LINES), so the guard is against unbounded growth,
    // not a hard 32-byte target: if a field pushes it past 48 bytes, move a rarely-used field
    // behind recompute-at-execute (or shrink the cache) rather than letting the line balloon.
    assert!(
        std::mem::size_of::<DecodedInsn>() <= 48,
        "DecodedInsn grew to {} bytes",
        std::mem::size_of::<DecodedInsn>()
    );
}

#[test]
fn decode_cache_hits_only_on_matching_tag_and_generation() {
    // A real decoded instruction to store. ADD AX, BX (01 D8).
    let (mut cpu, mem) = real_mode_cpu(&[0x01, 0xd8], 0x20);
    let mut bus = TestBus::with_memory(mem);
    cpu.registers.eip = 0;
    let insn = cpu.decode(&mut bus).unwrap();

    let mut cache = DecodeCache::new(4); // mask = 3
    let lin = 0x100;
    assert!(cache.get(lin, false).is_none(), "an empty line misses");
    cache.put(lin, insn, false, lin);
    assert!(cache.get(lin, false).is_some(), "a filled line hits");
    // Same line, queried under the other D bit: must miss (a 16-bit decode must never be
    // replayed in a 32-bit code segment; the D bit is part of the hit condition).
    assert!(
        cache.get(lin, true).is_none(),
        "a D-bit mismatch on a filled line misses"
    );
    // lin + 4 lands in the same direct-mapped slot (mask 3) but carries a different tag.
    assert!(
        cache.get(lin + 4, false).is_none(),
        "a different tag in the same slot misses (no false hit)"
    );
    cache.invalidate();
    assert!(
        cache.get(lin, false).is_none(),
        "a generation bump invalidates every stamped line"
    );
    cache.put(lin, insn, false, lin);
    assert!(
        cache.get(lin, false).is_some(),
        "re-filling after a bump hits again"
    );
}

#[test]
fn decode_cache_invalidate_skips_zero_on_wrap() {
    // The generation must never land back on 0 (a fresh line's default), or stale lines alias.
    let mut cache = DecodeCache::new(2);
    cache.generation = u32::MAX;
    cache.invalidate();
    assert_eq!(cache.generation, 1, "wrap skips 0");
}

#[test]
fn cycle_serves_a_repeated_instruction_from_the_decode_cache() {
    // INC AX (0x40): one byte, no branch, so re-executing at the same linear address is a hit.
    let (mut cpu, mem) = real_mode_cpu(&[0x40], 0x20);
    let mut bus = TestBus::with_memory(mem);
    let lin = cpu.linear_eip();

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "first run increments AX");
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "cycle caches the decoded instruction"
    );

    // Re-run at the same linear address: served from the cache, identical effect.
    cpu.set_eip(0);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 2, "cached INC AX runs again");

    // A CS load NEVER flushes the decode cache: the cache is linear-keyed, the D bit is in
    // the hit condition, and the fetch limit is re-checked live at each hit. This is the
    // pmode interrupt-edge / V86 monitor round-trip case that used to flush the whole cache
    // 326M times in a Doom timedemo.
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "a same-base CS reload keeps the decode cache"
    );
    cpu.load_segment_real(SegmentIndex::Cs, 0x100);
    assert!(
        cpu.decode_cache.get(lin, false).is_some(),
        "a changed-base CS load keeps the line too - the linear key still identifies it"
    );
}

#[test]
fn lock_prefixed_instructions_are_not_cached() {
    // LOCK ADD [BX], AL (F0 00 07). `decode` runs check_lock_target, which peeks the lock
    // target over the bus (charging clocks that are not part of `len`) and would #UD a
    // non-lockable target. A cached replay skips both, so a LOCK instruction must re-decode
    // every time and is never cached.
    let (mut cpu, mem) = real_mode_cpu(&[0xf0, 0x00, 0x07], 0x40);
    let mut bus = TestBus::with_memory(mem);
    cpu.registers.set_eax(1); // AL = 1
    cpu.registers.set_ebx(0x20);
    let lin = cpu.linear_eip();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(bus.memory[0x20], 1, "LOCK ADD [BX], AL executed");
    assert!(
        cpu.decode_cache.get(lin, false).is_none(),
        "a LOCK-prefixed instruction must not be cached (it re-charges + re-validates each run)"
    );
}

#[test]
fn cross_page_write_into_cached_code_invalidates_it() {
    // INC AX (0x40) at page 1; a store program at page 2 overwrites that byte with 0x48 (DEC
    // AX). Executing on a different page than the write is the cross-page SMC case begin_
    // instruction's current-page check cannot catch. The store program sits at 0x2008 so none
    // of its bytes collide with 0x1000's direct-mapped slot (slot 0); a collision would evict
    // the line and mask whether SMC actually invalidated it.
    let mut memory = vec![0u8; 0x3000];
    memory[0x1000] = 0x40; // INC AX
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x48; //   = 0x48 (DEC AX opcode)
    memory[0x200a] = 0xa2; // MOV moffs16, AL
    memory[0x200b] = 0x00;
    memory[0x200c] = 0x10; //   moffs = 0x1000
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    let mut bus = TestBus::with_memory(memory);

    // 1. Run INC AX at 0x1000: caches it and marks physical page 1 as code.
    cpu.registers.set_eax(0);
    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran");
    assert!(
        cpu.decode_cache.get(0x1000, false).is_some(),
        "0x1000 is cached"
    );

    // 2. From page 2, store 0x48 over the byte at 0x1000 (a write into the cached code page).
    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x48
    cpu.cycle(&mut bus).unwrap(); // MOV [0x1000], AL -> record_write_page bumps the generation
    assert!(
        cpu.decode_cache.get(0x1000, false).is_none(),
        "a write into the cached code page invalidated it"
    );

    // 3. Re-run at 0x1000: re-decodes the NEW opcode 0x48 (DEC AX), not the stale INC AX.
    cpu.registers.set_eax(5);
    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        4,
        "the freshly written DEC AX ran, not the stale cached INC AX"
    );
}

#[test]
fn data_write_to_a_non_code_page_does_not_flush_the_cache() {
    // The whole point of the code-page bitmap: a plain data write must NOT invalidate the cache,
    // or a write-heavy loop (dhrystone) would re-decode every iteration. Cache code on page 1,
    // run the store program on page 2 (at 0x2008 so it does not collide with 0x1000's slot),
    // write to page 3 (never executed), assert the line lives.
    let mut memory = vec![0u8; 0x4000];
    memory[0x1000] = 0x40; // INC AX at page 1
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x99;
    memory[0x200a] = 0xa2; // MOV moffs16, AL
    memory[0x200b] = 0x50;
    memory[0x200c] = 0x30; //   moffs = 0x3050 (page 3, holds no code)
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    let mut bus = TestBus::with_memory(memory);

    cpu.set_eip(0x1000);
    cpu.cycle(&mut bus).unwrap(); // cache INC AX, mark page 1
    assert!(cpu.decode_cache.get(0x1000, false).is_some());

    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x99
    cpu.cycle(&mut bus).unwrap(); // MOV [0x3050], AL -> page 3 is not a code page
    assert!(
        cpu.decode_cache.get(0x1000, false).is_some(),
        "a data write to a non-code page must not flush the decode cache"
    );
}

#[test]
fn a_cached_line_is_not_served_past_a_shrunken_cs_limit() {
    // A CS load no longer flushes the decode cache, so the fetch limit must be re-checked
    // live at every hit: cache INC AX at eip 0x10 under a 64 KB CS, reload CS with an
    // identical base/D but a limit BELOW 0x10, and re-enter. The line is still in the cache
    // (no flush) but must MISS to decode, which raises #GP on the out-of-limit fetch -- the
    // stale INC AX must never run.
    let mut memory = vec![0u8; 0x40];
    memory[0x10] = 0x40; // INC AX
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.set_eip(0x10);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran and was cached");
    assert!(cpu.decode_cache.get(0x10, false).is_some());

    // Same base and D, limit 0xF: eip 0x10 is now past the segment end.
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            limit: 0xf,
            ..cpu.registers.cs()
        },
    );
    cpu.invalidate_code_caches_for_cs_load();
    assert!(
        cpu.decode_cache.get(0x10, false).is_some(),
        "the line itself survives the CS load (no flush)"
    );
    cpu.registers.set_eax(5);
    cpu.set_eip(0x10);
    let _ = cpu.cycle(&mut bus); // #GP on the fetch (delivery may error: no IDT set up)
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        5,
        "the out-of-limit fetch faulted; the stale cached INC AX did NOT run"
    );
}

#[test]
fn isa_gate_exempt_decodes_are_never_cached() {
    // The firmware-ROM/ring-0 ISA-gate exemptions are context, not bytes. With CS loads no
    // longer flushing the decode cache, an exempt decode entering the cache could replay at
    // CPL 3 where the same bytes must #UD -- so both exemption channels must mark the
    // decode no-cache. Ring-0 protected mode at level I286, both channels:
    //   - 0F A2 CPUID: gated by is_386plus_two_byte at I286, passes via is_ring0_protected.
    //   - 66 40 (INC EAX): the 66 prefix #UDs at pre-386 outside the exemption.
    let mut memory = vec![0u8; 256];
    memory[..2].copy_from_slice(&[0x0f, 0xa2]); // CPUID at linear 0
    memory[0x10..0x12].copy_from_slice(&[0x66, 0x40]); // INC EAX at linear 0x10
    let mut cpu = Cpu386::default();
    cpu.set_level(CpuLevel::I286);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: false, // 16-bit segment: the 66 prefix is a real override
        },
    );
    let mut bus = TestBus::with_memory(memory);

    cpu.registers.eip = 0;
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.decode_cache.get(0, false).is_none(),
        "an exempt two-byte decode (CPUID at I286, ring 0) must not be cached"
    );

    cpu.set_eip(0x10);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.decode_cache.get(0x10, false).is_none(),
        "an exempt 66-prefixed decode (pre-386, ring 0) must not be cached"
    );
}

#[test]
fn smc_above_the_byte_bitmap_coverage_invalidates_via_page_marks() {
    // Stage-2 review finding 12: extended-memory code (where DOS-extender workloads live,
    // e.g. Quake's self-patching renderer) sits above SMC_BYTE_COVERAGE. The byte bitmap
    // does not reach there; the 4 KiB page marks must catch the write, or - now that CS
    // loads no longer flush - a stale line replays FOREVER. Same shape as
    // cross_page_write_into_cached_code_invalidates_it, relocated above 2 MiB.
    const HI: usize = 0x0020_1000; // 2 MiB + 4 KiB
    let mut memory = vec![0u8; 0x0020_3000];
    memory[HI] = 0x40; // INC AX above the byte coverage
    memory[0x2008] = 0xb0; // MOV AL, imm8
    memory[0x2009] = 0x48; //   = 0x48 (DEC AX opcode)
    memory[0x200a] = 0xa2; // MOV moffs, AL (moffs is 32-bit under the D=1 flat segment)
    memory[0x200b..0x200f].copy_from_slice(&(HI as u32).to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    // Real-mode 64 KB limits cannot reach 2 MiB; run flat (the pmode shape that matters).
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0x0008, 0x9b));
    cpu.registers
        .set_segment(SegmentIndex::Ds, SegmentRegister::flat(0x0010, 0x93));
    cpu.control.cr0 |= CR0_PE;
    let mut bus = TestBus::with_memory(memory);

    // 1. Run INC AX above 2 MiB: cached, page-marked.
    cpu.registers.set_eax(0);
    cpu.set_eip(HI as u32);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 1, "INC AX ran");
    assert!(cpu.decode_cache.get(HI as u32, true).is_some(), "cached");

    // 2. Store 0x48 (DEC AX) over it from low memory.
    cpu.set_eip(0x2008);
    cpu.cycle(&mut bus).unwrap(); // MOV AL, 0x48
    cpu.cycle(&mut bus).unwrap(); // MOV [HI], AL -> page mark hits -> generation bump
    assert!(
        cpu.decode_cache.get(HI as u32, true).is_none(),
        "a write into page-marked extended-memory code invalidated the cache"
    );

    // 3. Re-run: the NEW opcode (DEC AX) executes, not the stale INC AX.
    cpu.registers.set_eax(5);
    cpu.set_eip(HI as u32);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        4,
        "the freshly written DEC AX ran, not the stale cached INC AX"
    );
}

#[test]
fn d_bit_change_at_the_same_linear_address_re_decodes() {
    // The cache is keyed on the linear address, but a decode also depends on the code segment's
    // D bit (16- vs 32-bit operand/address size). MOV (E)AX, imm (0xB8) is 3 bytes in a 16-bit
    // segment (imm16) and 5 bytes in a 32-bit one (imm32). Caching the 16-bit form and then
    // aliasing the same linear with a 32-bit code segment must re-decode, not replay the 3-byte
    // form. A real protected-mode CS load routes through invalidate_code_caches; this drives
    // that effect directly (set the 32-bit CS, then invalidate) to avoid a full GDT setup.
    let mut memory = vec![0u8; 0x100];
    memory[0..5].copy_from_slice(&[0xb8, 0x34, 0x12, 0x78, 0x56]); // B8 imm
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0); // 16-bit
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);

    // 16-bit: MOV AX, 0x1234 (3 bytes).
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 0x1234);
    assert_eq!(cpu.registers.eip, 3);
    assert!(cpu.decode_cache.get(0, false).is_some());

    // Alias linear 0 with a 32-bit code segment (same base 0). NO flush happens or is
    // needed: the D bit is part of the hit condition, so the cached 16-bit decode simply
    // cannot hit under the 32-bit segment.
    let cs32 = SegmentRegister {
        default_size_32: true,
        ..cpu.registers.cs()
    };
    cpu.registers.set_segment(SegmentIndex::Cs, cs32);
    assert!(
        cpu.decode_cache.get(0, false).is_some(),
        "the 16-bit line itself stays cached"
    );
    assert!(
        cpu.decode_cache.get(0, true).is_none(),
        "but it can never be served to a 32-bit code segment"
    );

    // 32-bit: MOV EAX, 0x56781234 (5 bytes), not the stale 3-byte form.
    cpu.registers.set_eax(0);
    cpu.set_eip(0);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eax(),
        0x5678_1234,
        "a 32-bit immediate was read"
    );
    assert_eq!(
        cpu.registers.eip, 5,
        "the 32-bit MOV is 5 bytes, not the cached 3"
    );
}

#[test]
fn a_fetch_page_made_not_present_re_faults_after_invalidation() {
    // A cache hit must not execute an instruction whose fetch would now fault. Page linear
    // 0x1000 -> frame 0x5000 (present), cache INC AX there, then clear the PTE present bit and
    // flush (which bumps the decode generation). Re-entry must re-decode, fault on the absent
    // fetch page, and leave AX untouched -- never replay the cached INC AX. (Observed via AX
    // rather than cr2 because, with no IDT mapped, delivering the #PF cascades a second fault.)
    let mut memory = vec![0u8; 0x8000];
    memory[0x6000..0x6004].copy_from_slice(&0x0000_7007u32.to_le_bytes()); // PD[0] -> PT 0x7000
    memory[0x7004..0x7008].copy_from_slice(&0x0000_5007u32.to_le_bytes()); // PT[1] (lin 0x1000) -> 0x5000
    memory[0x5000] = 0x40; // INC AX at linear 0x1000
    let mut cpu = Cpu386::default();
    cpu.control.cr3 = 0x6000;
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.registers
        .set_segment(SegmentIndex::Cs, SegmentRegister::flat(0, 0x9b));
    cpu.registers.eip = 0x1000;
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap(); // INC AX runs (AX 0 -> 1), cached at linear 0x1000
    assert_eq!(cpu.registers.eax() & 0xffff, 1);
    assert!(cpu.decode_cache.get(0x1000, true).is_some());

    // Clear the PTE present bit and flush so the cache invalidates.
    bus.memory[0x7004..0x7008].copy_from_slice(&0x0000_5006u32.to_le_bytes());
    cpu.flush_tlb_and_code_caches();
    assert!(
        cpu.decode_cache.get(0x1000, true).is_none(),
        "the flush invalidated the cache"
    );

    cpu.registers.set_eax(5);
    cpu.set_eip(0x1000);
    let _ = cpu.cycle(&mut bus); // faults on the absent fetch page (delivery may error: no IDT)
    assert_eq!(
        cpu.registers.eax() & 0xffff,
        5,
        "the re-fetch from the now-absent page faulted; the stale cached INC AX did NOT run"
    );
}

#[test]
fn note_a20_changed_invalidates_the_decode_cache() {
    // A20 is masked at the bus, not the CPU, so toggling it changes which physical bytes back a
    // linear address near the 1 MB wrap without any CPU-visible state change. The machine calls
    // note_a20_changed on the transition so a cached decode of the old bytes is not replayed.
    let (mut cpu, mem) = real_mode_cpu(&[0x40], 0x20);
    let mut bus = TestBus::with_memory(mem);
    let lin = cpu.linear_eip();
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.decode_cache.get(lin, false).is_some());

    cpu.note_a20_changed();
    assert!(
        cpu.decode_cache.get(lin, false).is_none(),
        "an A20 toggle invalidates the decode cache"
    );
}

fn run_at_level(code: &[u8], level: CpuLevel) -> Result<CycleOutcome, InternalFault> {
    let mut memory = vec![0; 1024];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.set_level(level);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    // Route through the production split. Prefix-gating (66/67 at I286, the 0F two-byte ISA gate)
    // lives in `decode`, so the #UD-at-286 assertions hold on the same path the guest runs.
    exec_one_split(&mut cpu, &mut bus)
}

#[test]
fn movzx_is_undefined_opcode_at_286_but_runs_at_386() {
    // 0F B6 C3: MOVZX AX, BL. A 386 addition, so #UD at the 286 level and fine above it.
    let code = [0x0f, 0xb6, 0xc3];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "MOVZX must raise #UD at I286"
    );
    assert!(
        run_at_level(&code, CpuLevel::I386).is_ok(),
        "MOVZX must execute at I386"
    );
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn firmware_rom_cs_is_exempt_from_the_286_gate() {
    // MOVZX AX, BL is a 386 op: guest code at the 286 level #UDs on it, but the
    // BIOS ROM must keep running the full ISA so a lowered GSW mode never faults
    // firmware (Accept, interrupt service, boot). CS in the F-segment ROM
    // aperture (base 0xF0000) is the exemption.
    let code = [0x0f, 0xb6, 0xc3];
    assert!(
        matches!(
            run_at_level(&code, CpuLevel::I286).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ),
        "guest MOVZX must still #UD at I286"
    );
    let mut memory = vec![0u8; 0x10_0000];
    let base = 0x000F_0000usize;
    memory[base..base + code.len()].copy_from_slice(&code);
    let mut cpu = Cpu386::default();
    cpu.set_level(CpuLevel::I286);
    cpu.load_segment_real(SegmentIndex::Cs, 0xF000);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Bx, 0x0042);
    let mut bus = TestBus::with_memory(memory);
    // Drive through the split (MOVZX is a converted group now); the firmware-ROM exemption to
    // the 286 ISA gate lives in `decode`, which `exec_one_split` exercises. Confirm the op truly
    // ran (AX = zero-extended BL) rather than merely not faulting.
    assert!(
        exec_one_split(&mut cpu, &mut bus).is_ok(),
        "MOVZX fetched from BIOS ROM must run even at I286"
    );
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        0x0042,
        "MOVZX must have zero-extended BL into AX"
    );
}

#[test]
fn two_byte_convention_charges_the_second_byte_exactly_once() {
    // RDTSC (0F 31) is a two-byte op routed through `DecodeGroup::Misc` (it leaf-calls
    // `execute_two_byte`). The two-byte decode convention folds the second byte into
    // `insn.opcode` as 0x0F31 in `decode`, and the executor never re-reads it. Guard that
    // single-charge here: running RDTSC through the production split must advance eip past both
    // bytes, write a sane TSC into EDX:EAX, and charge exactly 3 instruction fetches (one
    // prefetch-window peek plus the two opcode bytes 0x0F and 0x31). A second-byte double-read in
    // the convention would push the fetch count past 3; nothing else in the file pins this
    // convention property for a 0F op so directly.
    let code = [0x0f, 0x31];
    let mut mem = vec![0u8; 64];
    mem[..code.len()].copy_from_slice(&code);

    let mut split = Cpu386::default();
    split.load_segment_real(SegmentIndex::Cs, 0);
    split.registers.eip = 0;
    split.elapsed_clocks = 42;
    let mut sbus = TestBus::with_memory(mem);
    exec_one_split(&mut split, &mut sbus).expect("RDTSC must run through the split convention");

    assert_eq!(
        split.registers.eip, 0x2,
        "eip must advance past both opcode bytes"
    );
    // RDTSC writes the running counter into EDX:EAX; with 42 clocks elapsed EAX reads it back.
    assert_eq!(split.registers.edx(), 0, "TSC high dword");
    assert_eq!(split.registers.eax(), 42, "TSC low dword = elapsed clocks");
    assert_eq!(
        seam_fetch_count(&sbus),
        3,
        "the convention must charge the second 0F byte exactly once (no re-read)"
    );
}

#[test]
fn throttled_286_raises_ud_for_an_unconverted_two_byte_opcode_via_the_new_gate() {
    // BSWAP EAX (0F C8) is a 486 addition and stays on Fallback. The ISA gate that #UDs it at
    // the 286 level now lives in `decode` (the shared convention point), not in execute_two_byte.
    // Proving an *un-converted* 0F op still #UDs confirms the gate did not get tied to the one
    // converted group.
    let code = [0x0f, 0xc8];
    assert!(
        matches!(
            run_at_level(&code, CpuLevel::I286).unwrap_err(),
            InternalFault::Exception { vector: 6, .. }
        ),
        "BSWAP must #UD at I286 through the new gate location"
    );
    assert!(
        run_at_level(&code, CpuLevel::I486).is_ok(),
        "BSWAP must run at I486"
    );
}

/// The single-byte opcode values that the production split does NOT hand to a real group: every
/// prefix byte `read_prefixes` consumes (the six segment overrides, the 66h/67h operand/address-
/// size prefixes, LOCK 0xF0, and REP/REPNE 0xF3/0xF2) and 0x0F (the two-byte escape), none of
/// which is an instruction on its own — `read_prefixes`/`decode` consume them before
/// `route_group` ever classifies the following opcode, so reaching them AS an opcode is a decode
/// bug — plus 0x63 (ARPL) and 0xF1 (ICEBP/INT1), which are genuinely unimplemented. Everything
/// else in the single-byte space is implemented and MUST route to a real group. This list is the
/// sole authority for "not routed as a single-byte opcode"; the coverage test below derives the
/// implemented set as its complement.
const UNIMPLEMENTED_SINGLE_BYTE: &[u8] = &[
    0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65, // segment-override prefix bytes
    0x66, 0x67, // operand-size / address-size prefix bytes
    0xf0, 0xf2, 0xf3, // LOCK / REPNE / REP prefix bytes
    0x0f, // two-byte (0F) escape: folded into 0x0F00 | second by `decode`, never routed bare
    0x63, // ARPL (unimplemented)
    0xf1, // ICEBP / INT1 (unimplemented)
];

/// True when the second byte of a 0F opcode names an IMPLEMENTED two-byte instruction. The
/// complement (within 0x00..=0xff) is the un-implemented 0F space that MUST stay on
/// `TwoByteFallback` and #UD. Built from the routed sets in `route_group` plus the no-operand
/// 0F ops `execute_two_byte` still handles directly (which `route_group` sends to `Misc`).
fn implemented_two_byte(second: u8) -> bool {
    // The 0F bytes `route_group` classifies into a real group (DataMove/Branch/BitManip/
    // CondMove/SystemSeg/Misc), mirrored exactly from the 0F arm of `route_group`.
    let routed = matches!(
        second,
        // MOVZX/MOVSX (DataMove)
        0xb6 | 0xb7 | 0xbe | 0xbf
        // Jcc near (Branch)
        | 0x80..=0x8f
        // BitManip
        | 0xa3 | 0xab | 0xb3 | 0xbb | 0xba | 0xbc | 0xbd | 0xa4 | 0xa5 | 0xac | 0xad
        | 0xb0 | 0xb1 | 0xc0 | 0xc1
        // CMOVcc / SETcc / IMUL (CondMove)
        | 0x40..=0x4f | 0x90..=0x9f | 0xaf
        // SystemSeg
        | 0x00 | 0x01 | 0x02 | 0x03 | 0x06 | 0x20 | 0x21 | 0x22 | 0x23 | 0xb2 | 0xb4 | 0xb5
        // no-operand system/serializing/CPU-id + CMPXCHG8B + BSWAP + PUSH/POP FS/GS (Misc)
        | 0x05 | 0x07 | 0x08 | 0x09 | 0x30 | 0x31 | 0x32 | 0xa0 | 0xa1 | 0xa2 | 0xa8 | 0xa9
        | 0xc7 | 0xc8..=0xcf
    );
    routed || is_mmx_two_byte(second)
}

#[test]
fn every_implemented_opcode_routes_off_the_legacy_fallback() {
    // Stage-A invariant lock: after the transitional fused fallback is gone, the production
    // `decode`/`execute_decoded` seam must hand EVERY implemented opcode to a dedicated split
    // group. `DecodeGroup::Fallback`/`TwoByteFallback` are the only two variants whose executor
    // raises `Unsupported{,TwoByte}Opcode`, so proving every implemented opcode routes to some
    // OTHER variant proves production never enters the dead-end fallback for a real instruction.
    //
    // Exhaustive partition (no representative sampling): classify the entire single-byte and
    // two-byte opcode space and check the implemented/unimplemented split against the authority
    // lists above. A future edit that drops an implemented opcode back to Fallback, or adds an
    // opcode without routing it, fails here.
    let prefixes = Prefixes::default();

    for byte in 0x00u16..=0xff {
        let unimplemented = UNIMPLEMENTED_SINGLE_BYTE.contains(&(byte as u8));
        let group = Cpu386::route_group(byte, prefixes);
        let is_fallback = matches!(group, DecodeGroup::Fallback);
        assert!(
            !matches!(group, DecodeGroup::TwoByteFallback),
            "single-byte opcode {byte:#04x} must never route to TwoByteFallback"
        );
        if unimplemented {
            assert!(
                is_fallback,
                "unimplemented single-byte opcode {byte:#04x} must stay on Fallback, got {group:?}"
            );
        } else {
            assert!(
                !is_fallback,
                "implemented single-byte opcode {byte:#04x} must route off Fallback to a real group"
            );
        }
    }

    for second in 0x00u16..=0xff {
        // `decode` folds the second byte into the opcode as 0x0F00 | second.
        let opcode = 0x0f00 | second;
        let group = Cpu386::route_group(opcode, prefixes);
        let is_two_byte_fallback = matches!(group, DecodeGroup::TwoByteFallback);
        assert!(
            !matches!(group, DecodeGroup::Fallback),
            "two-byte opcode 0F {second:#04x} must route via the 0F map, never plain Fallback"
        );
        if implemented_two_byte(second as u8) {
            assert!(
                !is_two_byte_fallback,
                "implemented two-byte opcode 0F {second:#04x} must route off TwoByteFallback to a real group"
            );
        } else {
            assert!(
                is_two_byte_fallback,
                "unimplemented two-byte opcode 0F {second:#04x} must stay on TwoByteFallback, got {group:?}"
            );
        }
    }
}

#[test]
fn fallback_path_is_reached_only_by_unimplemented_opcodes_and_still_uds() {
    // The runtime companion to the routing-partition test: drive each genuinely-unimplemented
    // opcode through the production split (`exec_one_split` -> `decode` -> `execute_decoded`) and
    // confirm the ONLY behavior the Fallback / TwoByteFallback arms produce is the exact
    // `Unsupported{,TwoByte}Opcode` #UD the legacy fused path produced — same error variant,
    // carrying the same `cs`. This proves the fallback arms are a pure dead-end for real
    // instructions: nothing implemented can reach them, and the unimplemented ones still #UD.
    for &op in UNIMPLEMENTED_SINGLE_BYTE {
        // The eight prefix bytes are valid as prefixes; they only #UD when they are the whole
        // instruction (no following opcode), which `read_prefixes` reaches end-of-stream on at
        // I286 but treats as a real prefix at I586. To exercise the Fallback opcode arm we need a
        // byte that is an *opcode*, never a prefix: ARPL (0x63) and ICEBP (0xf1). The prefix
        // bytes are covered by the routing-partition test above and the dedicated #UD guards.
        if matches!(op, 0x63 | 0xf1) {
            let mut cpu = Cpu386::default();
            cpu.load_segment_real(SegmentIndex::Cs, 0);
            cpu.registers.eip = 0;
            let mut bus = TestBus::with_memory(vec![op, 0, 0, 0]);
            let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
            assert!(
                matches!(
                    err,
                    InternalFault::Exception {
                        vector: 6,
                        error_code: None
                    }
                ),
                "single-byte opcode {op:#04x} must #UD, got {err:?}"
            );
        }
    }

    // A representative un-implemented 0F byte that falls through to the generic catch-all:
    // 0x0a (unmapped). It routes to TwoByteFallback and #UDs as UnsupportedTwoByteOpcode. (0F
    // B2/B4/B5 LSS/LFS/LGS and 0F 21/23 MOV reg,DR / MOV DR,reg are now implemented -- see the
    // SystemSeg coverage test below and the ledger-row-25 `mov_dr*` tests. 0F AA RSM also routes
    // to TwoByteFallback but is an EXPLICITLY handled arm in `execute_two_byte` that #UDs with
    // vector 6 because no SMM is modeled -- it is "implemented", just always invalid -- so it is
    // not part of this generic-catch-all sweep.)
    let second = 0x0au8;
    assert!(
        !implemented_two_byte(second),
        "test bug: 0F {second:#04x} is actually implemented"
    );
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0x0f, second, 0xc0, 0, 0]);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "two-byte opcode 0F {second:#04x} must #UD, got {err:?}"
    );
}

#[test]
fn single_byte_f1_is_an_undefined_opcode() {
    // 0xF1 (ICEBP / INT1) is not implemented as a single-byte opcode. It must #UD through the
    // production split exactly like ARPL (0x63): `route_group` leaves it on Fallback and the
    // Fallback arm raises UnsupportedOpcode. Dedicated guard alongside the ARPL/prefix-byte
    // #UD tests so a future edit that mis-routes 0xF1 is caught here.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(vec![0xf1, 0, 0, 0]);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "0xF1 must raise #UD, got {err:?}"
    );
}

#[test]
fn operand_size_prefix_is_undefined_opcode_at_286() {
    // 66 B8 ... a 32-bit MOV EAX, imm32 reached through the operand-size prefix. The 286
    // has no 66h prefix, so the decoder #UDs on the prefix byte; 386 and up run it.
    let code = [0x66, 0xb8, 0x78, 0x56, 0x34, 0x12];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "the 66h operand-size prefix must raise #UD at I286"
    );
    assert!(run_at_level(&code, CpuLevel::I386).is_ok());
}

#[test]
fn address_size_prefix_is_undefined_opcode_at_286() {
    // 67 prefix: a 32-bit address form. Absent on the 286, present from the 386.
    // 67 8B 00 would be MOV with a 32-bit address; the prefix alone must #UD at I286.
    let code = [0x67, 0x90];
    let fault = run_at_level(&code, CpuLevel::I286).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
    // At I386 the prefix decodes and the NOP after it runs.
    assert!(run_at_level(&code, CpuLevel::I386).is_ok());
}

#[test]
fn shld_and_setcc_are_undefined_opcodes_at_286() {
    // 0F A4 SHLD and 0F 90 SETO are both 386 additions.
    let shld = [0x0f, 0xa4, 0xc3, 0x04]; // SHLD BX, AX, 4
    let setcc = [0x0f, 0x90, 0xc0]; // SETO AL
    for code in [&shld[..], &setcc[..]] {
        let fault = run_at_level(code, CpuLevel::I286).unwrap_err();
        assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
        assert!(run_at_level(code, CpuLevel::I386).is_ok());
    }
}

#[test]
fn lgdt_still_runs_at_286() {
    // 0F 01 /2 LGDT is a 286 instruction, so it must NOT be gated at the 286 level.
    // ModRM 16 = mode 0, reg 2 (LGDT), rm 6 (direct disp16) pointing at a 6-byte pseudo-
    // descriptor in memory.
    let mut memory = vec![0; 1024];
    memory[0..4].copy_from_slice(&[0x0f, 0x01, 0x16, 0x20]); // disp16 = 0x0020
    // 6-byte GDTR image at 0x0020: limit then 32-bit base.
    memory[0x20..0x26].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    let mut cpu = Cpu386::default();
    cpu.set_level(CpuLevel::I286);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    // LGDT (0F 01 /2) is converted to the decode/execute split (task A12); run it through the
    // split, not the legacy fused entry (whose 0F 01 arm is gone).
    assert!(exec_one_split(&mut cpu, &mut bus).is_ok());
    assert_eq!(cpu.gdtr.limit, 0x00ff);
    assert_eq!(cpu.gdtr.base, 0x0000_1000);
}

#[test]
fn cpuid_is_undefined_opcode_below_486() {
    // CPUID is absent on the 286 and 386; it appears on the 486 and 586.
    let code = [0x0f, 0xa2];
    assert!(matches!(
        run_at_level(&code, CpuLevel::I286).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(matches!(
        run_at_level(&code, CpuLevel::I386).unwrap_err(),
        InternalFault::Exception { vector: 6, .. }
    ));
    assert!(run_at_level(&code, CpuLevel::I486).is_ok());
    assert!(run_at_level(&code, CpuLevel::I586).is_ok());
}

#[test]
fn cpuid_runs_in_ring0_protected_mode_below_486() {
    // The exec-time CPUID gate carries the same ring-0 protected-mode
    // exemption as the prefix and 0F-extended gates: chipset-side ring-0
    // monitor code gets the full core ISA even when the guest persona has
    // no CPUID (I286/I386). Same flat CPL-0 code segment shape as
    // `cpl3_code`, but with an RPL-0 selector. I386 is the interesting
    // level: the I286 two-byte gate does not apply, so only the CPUID
    // gate decides. (At I486/I586 `has_cpuid()` is true and the gate is
    // moot for everyone.)
    let mut memory = vec![0u8; 256];
    memory[..2].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = Cpu386::default();
    cpu.set_level(CpuLevel::I386);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0008,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    let mut bus = TestBus::with_memory(memory);
    assert!(
        exec_one_split(&mut cpu, &mut bus).is_ok(),
        "CPUID must execute in ring-0 protected mode at the I386 level"
    );

    // Guest-facing code is still gated: CPL-3 protected mode at I386 #UDs.
    let (mut cpu3, mut bus3) = cpl3_code(&[0x0f, 0xa2]);
    cpu3.set_level(CpuLevel::I386);
    let fault = exec_one_split(&mut cpu3, &mut bus3).unwrap_err();
    assert!(
        matches!(fault, InternalFault::Exception { vector: 6, .. }),
        "CPUID must still #UD for CPL-3 guest code at the I386 level"
    );
    // (Real-mode #UD at I286/I386 is pinned by cpuid_is_undefined_opcode_below_486.)
}

#[test]
fn cpuid_extended_leaf1_reports_amd_feature_flags() {
    let cpu = run_cpuid(0x8000_0001);
    // EAX carries the processor signature: family 5.
    assert_eq!((cpu.registers.eax() >> 8) & 0xf, 5);
    let edx = cpu.registers.edx();
    // The implemented instructions sit at their AMD extended-leaf bit positions.
    assert_ne!(edx & (1 << 10), 0, "SYSCALL/SYSRET (bit 10)");
    assert_ne!(edx & (1 << 15), 0, "integer CMOVcc (bit 15)");
    assert_ne!(edx & (1 << 16), 0, "FP FCMOVcc (bit 16)");
    assert_ne!(edx & (1 << 4), 0, "TSC");
    assert_ne!(edx & (1 << 5), 0, "MSR");
    assert_ne!(edx & (1 << 8), 0, "CX8");
    assert_ne!(edx & (1 << 23), 0, "MMX");
    // Features the GSW-586 does not emulate stay clear.
    assert_eq!(edx & 1, 0, "FPU off");
    assert_eq!(edx & (1 << 7), 0, "no machine-check exception");
}

#[test]
fn cpuid_cache_leaves_report_level_sizes() {
    // The AMD-style L1 (0x80000005) and L2 (0x80000006) leaves carry the live level's
    // cache sizes in ECX: L1 KB in bits 31-24, L2 KB in bits 31-16.
    let mut cpu = run_cpuid(0x8000_0005);
    assert_eq!(cpu.registers.ecx() >> 24, 32); // I586 L1 = 32 KB (P55C: 16K I + 16K D)
    cpu = run_cpuid(0x8000_0006);
    assert_eq!(cpu.registers.ecx() >> 16, 512); // I586 L2 = 512 KB
}

#[test]
fn id_flag_toggle_detection_sequence_finds_cpuid() {
    // The standard CPUID-presence probe: read EFLAGS, flip ID (bit 21), write it back,
    // read EFLAGS again, and conclude CPUID exists if ID changed. Model that here using
    // PUSHFD/POPFD plus a software toggle of FLAG_ID, then run CPUID leaf 0 to confirm
    // the detection concludes correctly.
    let mut memory = vec![0; 1024];
    // 66 9c PUSHFD ; 66 9d POPFD to round-trip the dword image carrying ID.
    memory[0..2].copy_from_slice(&[0x66, 0x9c]);
    memory[2..4].copy_from_slice(&[0x66, 0x9d]);
    // 0f a2 CPUID with EAX = 0 already loaded.
    memory[4..6].copy_from_slice(&[0x0f, 0xa2]);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0100);
    cpu.registers.set_eax(0);
    let mut bus = TestBus::with_memory(memory);

    // Establish ID = 0, flip it on, and confirm the flag image carries the change so a
    // detection routine would observe ID as toggleable (CPUID present).
    let before = cpu.flag(FLAG_ID);
    cpu.set_flag(FLAG_ID, !before);
    cpu.cycle(&mut bus).unwrap(); // pushfd captures ID = 1
    cpu.set_flag(FLAG_ID, before); // perturb
    cpu.cycle(&mut bus).unwrap(); // popfd restores ID = 1
    let toggled = cpu.flag(FLAG_ID);
    assert_eq!(toggled, !before, "ID flag must be toggleable");

    // Detection concluded CPUID is present; execute it and confirm the GSW-586 vendor.
    cpu.cycle(&mut bus).unwrap(); // cpuid
    assert_eq!(cpu.registers.eax(), 1);
    assert_eq!(cpu.registers.ebx().to_le_bytes(), *b"Genu");
}

// ---- Slice 1: real-mode integer opcode completion (see dev_docs/COVERAGE.md) ----

fn real_mode_cpu(code: &[u8], mem_len: usize) -> (Cpu386, Vec<u8>) {
    let mut memory = vec![0u8; mem_len];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    (cpu, memory)
}

/// Drive `run_straight_line` repeatedly the way the machine batch loop does (without devices or
/// interrupts): keep starting a fresh run from the current eip until one halts or a generous step
/// budget is exhausted. Returns the number of runs the executor produced, so a test can assert a
/// hot loop actually collapsed into multi-instruction runs rather than one-instruction stutters.
fn drive_straight_line_runs(cpu: &mut Cpu386, bus: &mut TestBus) -> usize {
    let mut runs = 0;
    for _ in 0..10_000 {
        runs += 1;
        let outcome = cpu.run_straight_line(bus, u64::MAX).unwrap();
        if outcome.halted {
            return runs;
        }
    }
    panic!("straight-line driver never halted");
}

#[test]
fn straight_line_hot_loop_matches_per_instruction_result() {
    // MOV CX,5 ; loop: INC AX ; INC AX ; LOOP loop ; HLT. Once the loop body is cached, the
    // relative LOOP can run as a continuation too, so one hot run can chain several iterations.
    let code = [
        0xb9, 0x05, 0x00, // MOV CX, 5
        0x40, // INC AX            (loop target, 0x03)
        0x40, // INC AX            (0x04)
        0xe2, 0xfc, // LOOP -4 -> 0x03  (0x05)
        0xf4, // HLT               (0x07)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    let runs = drive_straight_line_runs(&mut cpu, &mut bus);
    // Two warming INCs (cache cold) plus four loop iterations of two INCs each = 10.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 10);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    // 17 instructions retire: MOV CX, 2 warming INCs, then 5 LOOP iterations (the body's 2 INCs
    // run on the four jumping iterations, the fifth LOOP falls through), and HLT =
    // 1 + 2 + (5 LOOP + 4*2 INC) + 1 = 17. A one-instruction-per-run executor would produce 17
    // runs; with cached branch continuations this cold-start case reaches HLT in five runner
    // entries: MOV miss, first INC miss, second INC miss, the hot chained loop, then HLT.
    let retired = 1 + 2 + (5 + 4 * 2) + 1;
    assert!(
        runs < retired,
        "the hot loop must collapse into multi-instruction runs: {runs} runs for {retired} \
             instructions"
    );
    assert_eq!(runs, 5, "cached LOOP should stay inside the hot run");
}

#[test]
fn straight_line_run_executes_hot_register_cached_forms() {
    // MOV AX,1 ; TEST AX,AX ; JNZ target ; MOV BX,dead ; target: MOV CX,2 ; MOV BX,AX ;
    // MOV AX,BX ; DEC CX ; HLT. The warm second run exercises cached continuation fast paths
    // for TEST reg/reg, JNZ, MOV reg,imm, both MOV reg/reg directions, and DEC reg. The skipped
    // MOV proves the branch target, not the contiguous bytes, drives the next continuation.
    let code = [
        0xb8, 0x01, 0x00, // MOV AX, 1
        0x85, 0xc0, // TEST AX, AX
        0x75, 0x03, // JNZ +3 -> 0x0A
        0xbb, 0xad, 0xde, // MOV BX, 0xDEAD (skipped)
        0xb9, 0x02, 0x00, // MOV CX, 2
        0x89, 0xc3, // MOV BX, AX
        0x8b, 0xc3, // MOV AX, BX
        0x49, // DEC CX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "HLT remains a terminator");
    assert_eq!(cpu.registers.eip, 0x12, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 1, "taken JNZ skipped dead MOV");
    assert_eq!(cpu.read_reg16(Reg16::Cx), 1, "MOV imm then DEC ran");
    assert!(!cpu.flag(FLAG_ZF), "DEC CX from 2 to 1 leaves ZF clear");
}

#[test]
fn straight_line_cached_zf_branches_keep_test_flags_lazy() {
    for (opcode, ax) in [(0x74, 0u8), (0x75, 1u8)] {
        // MOV AX,ax ; TEST AX,AX ; JZ/JNZ target ; MOV BX,dead ; target: MOV BX,1234 ; HLT.
        let code = [
            0xb8, ax, 0x00, // MOV AX, ax
            0x85, 0xc0, // TEST AX, AX
            opcode, 0x03, // JZ/JNZ +3 -> target
            0xbb, 0xad, 0xde, // MOV BX, 0xDEAD (skipped)
            0xbb, 0x34, 0x12, // target: MOV BX, 0x1234
            0xf4, // HLT
        ];
        let (mut cpu, memory) = real_mode_cpu(&code, 1024);
        let mut bus = TestBus::with_memory(memory);
        drive_straight_line_runs(&mut cpu, &mut bus);

        cpu.registers.eip = 0;
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.pending_flags = PendingFlags::default();
        cpu.halted = false;
        cpu.reset_perf_counters();

        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted, "HLT remains the run terminator");
        assert_eq!(cpu.registers.eip, 0x0d, "run stopped at HLT");
        assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234, "branch was taken");
        assert!(
            cpu.pending_flags.tag & (1u32 << 31) != 0,
            "TEST flags should remain deferred after JZ/JNZ reads ZF"
        );
        assert_eq!(
            cpu.perf.flag_materializations, 0,
            "JZ/JNZ should read pending ZF without materializing"
        );
    }
}

#[test]
fn hot_cached_forms_do_not_alias_two_byte_opcodes() {
    // 0F 44 C3 is CMOVE AX,BX. Its low byte is 0x44, which is INC SP in the single-byte map; the
    // hot cached single-byte table must not see it. With ZF clear, CMOVE leaves AX and SP alone.
    let code = [
        0xb8, 0x05, 0x00, // MOV AX, 5
        0xbb, 0x03, 0x00, // MOV BX, 3
        0x0f, 0x44, 0xc3, // CMOVE AX, BX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Bx, 0);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.set_flag(FLAG_ZF, false);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 5, "CMOVE false leaves AX");
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0100, "CMOVE is not INC SP");
}

#[test]
fn straight_line_run_executes_hot_alu_group_cached_forms() {
    // MOV AX,10 ; MOV BX,3 ; ADD AX,BX ; SUB AX,1 ; CMP AX,12 ; JNZ dead ;
    // OR AL,1 ; XOR AL,1 ; AND AX,0x00ff ; SHL AX,1 ; SHR AX,1 ; HLT. The warm second run
    // exercises cached ALU reg/reg, accumulator immediate, group-1 and group-2 register forms,
    // CMP no-writeback, and flags.
    let code = [
        0xb8, 0x0a, 0x00, // MOV AX, 10
        0xbb, 0x03, 0x00, // MOV BX, 3
        0x01, 0xd8, // ADD AX, BX
        0x83, 0xe8, 0x01, // SUB AX, 1
        0x83, 0xf8, 0x0c, // CMP AX, 12
        0x75, 0x07, // JNZ dead (not taken)
        0x0c, 0x01, // OR AL, 1
        0x34, 0x01, // XOR AL, 1
        0x25, 0xff, 0x00, // AND AX, 0x00ff
        0xd1, 0xe0, // SHL AX, 1
        0xd1, 0xe8, // SHR AX, 1
        0xf4, // HLT
        0xb8, 0xad, 0xde, // dead: MOV AX, 0xDEAD
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x1b, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x000c);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 3);
    assert!(!cpu.flag(FLAG_ZF), "final AND leaves a nonzero result");
    assert!(!cpu.flag(FLAG_SF));
}

#[test]
fn straight_line_run_executes_hot_memory_alu_group_cached_forms() {
    // MOV SI,0x40 ; MOV AX,4 ; MOV BX,3 ; MOV CL,0x7f ; MOV [SI],AX ; MOV [SI+2],CL ;
    // ADD [SI],BX ; SUB BX,[SI] ; ADD byte [SI+2],1 ; ADD DL,[SI+2] ;
    // ADD word [SI],5 ; CMP word [SI],12 ; JNZ dead ; MOV AX,[SI] ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x04, 0x00, // MOV AX, 4
        0xbb, 0x03, 0x00, // MOV BX, 3
        0xb1, 0x7f, // MOV CL, 0x7f
        0x89, 0x04, // MOV [SI], AX
        0x88, 0x4c, 0x02, // MOV [SI+2], CL
        0x01, 0x1c, // ADD [SI], BX
        0x2b, 0x1c, // SUB BX, [SI]
        0x80, 0x44, 0x02, 0x01, // ADD byte [SI+2], 1
        0x02, 0x54, 0x02, // ADD DL, [SI+2]
        0x83, 0x04, 0x05, // ADD word [SI], 5
        0x83, 0x3c, 0x0c, // CMP word [SI], 12
        0x75, 0x03, // JNZ dead (not taken)
        0x8b, 0x04, // MOV AX, [SI]
        0xf4, // HLT
        0xb8, 0xad, 0xde, // dead: MOV AX, 0xDEAD
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    bus.memory[0x40..0x43].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x25, "run stopped at HLT");
    assert_eq!(u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]), 12);
    assert_eq!(bus.memory[0x42], 0x80);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 12);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0xfffc);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0080);
    assert!(cpu.flag(FLAG_ZF), "CMP equal keeps the dead branch untaken");
}

#[test]
fn straight_line_run_executes_hot_datamove_cached_forms() {
    // MOV AX,0x00fe ; MOV BX,0x1234 ; MOV DI,4 ; MOVSX CX,AL ; MOVZX DX,BL ;
    // XCHG AX,BX ; LEA SI,[BX+DI+5] ; HLT.
    let code = [
        0xb8, 0xfe, 0x00, // MOV AX, 0x00fe
        0xbb, 0x34, 0x12, // MOV BX, 0x1234
        0xbf, 0x04, 0x00, // MOV DI, 4
        0x0f, 0xbe, 0xc8, // MOVSX CX, AL
        0x0f, 0xb6, 0xd3, // MOVZX DX, BL
        0x93, // XCHG AX, BX
        0x8d, 0x71, 0x05, // LEA SI, [BX+DI+5]
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    cpu.write_reg16(Reg16::Di, 0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x13, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x00fe);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0xfffe);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x0034);
    assert_eq!(cpu.read_reg16(Reg16::Si), 0x0107);
}

#[test]
fn straight_line_run_executes_hot_datamove_memory_cached_forms() {
    // MOV SI,0x40 ; MOV AX,0x1234 ; MOV [SI],AX ; MOV BX,[SI] ;
    // MOV CL,0x7f ; MOV [SI+2],CL ; MOV DL,[SI+2] ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x34, 0x12, // MOV AX, 0x1234
        0x89, 0x04, // MOV [SI], AX
        0x8b, 0x1c, // MOV BX, [SI]
        0xb1, 0x7f, // MOV CL, 0x7f
        0x88, 0x4c, 0x02, // MOV [SI+2], CL
        0x8a, 0x54, 0x02, // MOV DL, [SI+2]
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    bus.memory[0x40..0x43].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x12, "run stopped at HLT");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x1234
    );
    assert_eq!(bus.memory[0x42], 0x7f);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x007f);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0x007f);
}

#[test]
fn straight_line_run_executes_hot_flags_misc_cached_forms() {
    // MOV SI,0x40 ; MOV AX,0x8001 ; MOV [SI],AX ; TEST [SI],AX ;
    // MOV AL,0x80 ; CBW ; CWD ; CLC ; STC ; CMC ; CLD ; STD ;
    // MOV AH,0xd7 ; SAHF ; LAHF ; HLT.
    let code = [
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xb8, 0x01, 0x80, // MOV AX, 0x8001
        0x89, 0x04, // MOV [SI], AX
        0x85, 0x04, // TEST [SI], AX
        0xb0, 0x80, // MOV AL, 0x80
        0x98, // CBW
        0x99, // CWD
        0xf8, // CLC
        0xf9, // STC
        0xf5, // CMC
        0xfc, // CLD
        0xfd, // STD
        0xb4, 0xd7, // MOV AH, 0xd7
        0x9e, // SAHF
        0x9f, // LAHF
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Si, 0);
    cpu.registers.eflags = 0x02;
    cpu.pending_flags = PendingFlags::default();
    bus.memory[0x40..0x42].fill(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x17, "run stopped at HLT");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x40], bus.memory[0x41]]),
        0x8001
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xd780);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.eflags(), 0x4d7);
}

#[test]
fn straight_line_run_executes_hot_stack_cached_forms() {
    // MOV AX,0x1234 ; PUSH AX ; POP BX ; PUSH 0x55aa ; POP CX ; PUSH -1 ; POP DX ; HLT.
    // The warm second run keeps stack memory access on the existing push/pop helpers while
    // skipping the decoded stack dispatch for register and immediate forms.
    let code = [
        0xb8, 0x34, 0x12, // MOV AX, 0x1234
        0x50, // PUSH AX
        0x5b, // POP BX
        0x68, 0xaa, 0x55, // PUSH 0x55aa
        0x59, // POP CX
        0x6a, 0xff, // PUSH -1
        0x5a, // POP DX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.registers.set_ebx(0);
    cpu.registers.set_ecx(0);
    cpu.registers.set_edx(0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.eip, 0x0c, "run stopped at HLT");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Bx), 0x1234);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0x55aa);
    assert_eq!(cpu.read_reg16(Reg16::Dx), 0xffff);
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x0200);
}

#[test]
fn straight_line_run_sees_self_modified_later_instruction() {
    // The key correctness property of the lean executor: a guest write that modifies a later,
    // already-cached instruction must make the NEXT continuation re-decode the new bytes, never
    // replay the stale cached opcode. We loop so the body is cached, then on the second iteration
    // an early store overwrites a later instruction's opcode in place.
    //
    // Layout (DS = CS = 0):
    //   0x00: B9 02 00        MOV CX, 2
    //   loop (0x03):
    //   0x03: C6 06 0A 00 48  MOV byte [0x0A], 0x48   ; patch the op at 0x0A to DEC AX (0x48)
    //   0x08: 40              INC AX
    //   0x09: 40              INC AX                  ; cached as INC AX on pass 1, runs DEC on 2
    //   0x0A: 40              INC AX  <- patched to 0x48 (DEC AX) by the store at 0x03
    //   0x0B: E2 F6           LOOP -10 -> 0x03
    //   0x0D: F4              HLT
    //
    // Pass 1 (cache cold): the store writes 0x48 over [0x0A] BEFORE 0x0A is ever decoded, so 0x0A
    //   decodes fresh as DEC AX. AX: +1 (0x08) +1 (0x09) -1 (0x0A) = +1.
    // Pass 2 (body cached): 0x0A is now cached as DEC AX from pass 1. The store rewrites the same
    //   0x48, hitting a cached code byte -> generation bump -> the 0x0A continuation re-decodes
    //   (still DEC AX). AX: +1 +1 -1 = +1. Total AX = 2, CX = 0.
    // If the executor replayed the stale cache without honoring the SMC bump, 0x0A would run as
    //   the original INC and AX would be wrong.
    let code = [
        0xb9, 0x02, 0x00, // MOV CX, 2
        0xc6, 0x06, 0x0a, 0x00, 0x48, // MOV byte [0x000A], 0x48   (0x03)
        0x40, // INC AX                                  (0x08)
        0x40, // INC AX                                  (0x09)
        0x40, // INC AX  (patched to DEC AX at runtime)  (0x0A)
        0xe2, 0xf6, // LOOP -10 -> 0x03                  (0x0B)
        0xf4, // HLT                                     (0x0D)
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);
    // Each of the two iterations nets +1 because the patched op ran as DEC AX, not the stale INC.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    // The byte at 0x0A is the patched DEC AX opcode.
    assert_eq!(bus.memory[0x0a], 0x48);
}

#[test]
fn straight_line_run_discards_cached_opcode_overwritten_to_a_different_op() {
    // The strongly discriminating SMC case: a later instruction is cached as one opcode (INC AX,
    // a +1), an earlier guest store then overwrites it with a DIFFERENT opcode (DEC AX, a -1), and
    // the executor must re-decode the new opcode rather than replay the stale cached form. Because
    // the cached form (INC, +1) and the rewritten form (DEC, -1) have OPPOSITE effects, a stale
    // snapshot replay produces the wrong sign and the assertion fails - unlike a rewrite to the
    // already-cached value, which a stale replay would pass.
    //
    // Layout (DS = CS = SS = 0):
    //   0x00: C6 06 05 00 48   MOV byte [0x05], 0x48   ; store: patch P (0x05) from 0x40 to 0x48
    //   P = 0x05: 40           INC AX                  ; cached as INC AX first; 0x48 = DEC AX after
    //   0x06: F4               HLT
    let code = [
        0xc6, 0x06, 0x05, 0x00, 0x48, // MOV byte [0x0005], 0x48
        0x40, // INC AX  (P, patched to 0x48 = DEC AX at runtime)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);

    // Cache P as INC AX BEFORE any rewrite: run a single instruction starting at P so it decodes
    // and caches as INC AX (0x40). This is the cached form a stale replay would later wrongly use.
    cpu.registers.eip = 0x05;
    cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(
        cpu.decode_cache.get(0x05, false).is_some(),
        "P must be cached as INC AX before the rewrite"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1, "the warm INC ran once");

    // Now run from the top: the store overwrites P's opcode byte (0x40 -> 0x48). That write hits a
    // cached code byte, bumping the decode-cache generation, so when control reaches P it
    // re-decodes the NEW byte (DEC AX) instead of replaying the cached INC.
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    drive_straight_line_runs(&mut cpu, &mut bus);
    // DEC AX ran (AX = 0xFFFF). A stale-snapshot executor that replayed the cached INC AX would
    // leave AX = 0x0001, failing this assertion - that is the discrimination the L1 test lacked.
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        0xffff,
        "the rewritten DEC AX must run, not the stale cached INC AX"
    );
    assert_eq!(
        bus.memory[0x05], 0x48,
        "P's opcode byte was patched to DEC AX"
    );
}

#[test]
fn straight_line_run_stops_at_a_page_crossing_instruction() {
    // The continuation rule `(lin & 0xfff) + len <= 0x1000` keeps a run from executing a cached
    // instruction that would straddle a 4 KB page boundary; that instruction must run through the
    // normal path instead. This exercises BOTH sides of the `<= 0x1000` bound:
    //   - an instruction ENDING exactly at 0x1000 is allowed and runs as a continuation;
    //   - an instruction CROSSING 0x1000 ends the run and runs afterward via the normal path.
    // Real-mode flat layout (CS base 0, so lin == eip). The probe instruction is MOV AL, 7
    // (0xB0 0x07), a 2-byte straight-line DataMove with an observable effect (AL = 7).

    // Case A: the probe begins at 0xFFE and ends at 0x1000 (0xFFE + 2 == 0x1000) -> ALLOWED.
    //   0xFFD: 40         INC AX
    //   0xFFE: B0 07       MOV AL, 7   (ends exactly at 0x1000)
    //   0x1000: F4         HLT
    {
        let mut memory = vec![0u8; 0x2000];
        memory[0xffd] = 0x40; // INC AX
        memory[0xffe] = 0xb0; // MOV AL,
        memory[0xfff] = 0x07; //   7
        memory[0x1000] = 0xf4; // HLT
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        let mut bus = TestBus::with_memory(memory);

        // Warm the decode cache for all three instructions so the only thing gating a continuation
        // is the page check, not a cache miss.
        cpu.registers.eip = 0xffd;
        drive_straight_line_runs(&mut cpu, &mut bus);

        // Run once from 0xFFD: INC is the run's first instruction, then the cached MOV AL,7 (ending
        // exactly at 0x1000) runs as a continuation in the SAME run.
        cpu.registers.eip = 0xffd;
        cpu.registers.set_eax(0);
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        // The MOV ran as a continuation: AL == 7 and the run advanced past it (eip past 0x1000 is
        // the HLT, which ends the run as a non-straight-line group, leaving eip at 0x1000).
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            7,
            "MOV AL,7 ending at 0x1000 must run"
        );
        assert_eq!(
            cpu.registers.eip, 0x1000,
            "the run reached the HLT after the MOV"
        );
    }

    // Case B: the probe begins at 0xFFF and ends at 0x1001 (0xFFF + 2 > 0x1000) -> CROSSES.
    //   0xFFE: 40         INC AX
    //   0xFFF: B0 07       MOV AL, 7   (straddles the page boundary)
    //   0x1001: F4         HLT
    {
        let mut memory = vec![0u8; 0x2000];
        memory[0xffd] = 0x40; // INC AX (warm anchor)
        memory[0xffe] = 0x40; // INC AX
        memory[0xfff] = 0xb0; // MOV AL,
        memory[0x1000] = 0x07; //   7  (this byte is on the next page)
        memory[0x1001] = 0xf4; // HLT
        let mut cpu = Cpu386::default();
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        let mut bus = TestBus::with_memory(memory);

        // Warm all instructions (INC 0xFFD, INC 0xFFE, MOV 0xFFF, HLT 0x1001) into the cache.
        cpu.registers.eip = 0xffd;
        drive_straight_line_runs(&mut cpu, &mut bus);
        assert!(
            cpu.decode_cache.get(0xfff, false).is_some(),
            "the page-crossing MOV must be cached, so only the page check can stop the run"
        );

        // Run once from 0xFFD: INC (0xFFD, first) + INC (0xFFE, continuation) run, then the cached
        // MOV at 0xFFF is REJECTED by the page check (0xFFF + 2 > 0x1000) and the run STOPS there.
        cpu.registers.eip = 0xffd;
        cpu.registers.set_eax(0);
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        assert_eq!(cpu.read_reg16(Reg16::Ax), 2, "the two INCs ran");
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            2,
            "the page-crossing MOV did NOT run in this call (AL is not 7)"
        );
        assert_eq!(
            cpu.registers.eip, 0xfff,
            "the run stopped at the page-crossing MOV"
        );

        // The crossing MOV runs correctly afterward through the normal path (first instruction of
        // the next run, not subject to the continuation page check).
        let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
        assert!(!outcome.halted);
        assert_eq!(
            cpu.read_reg16(Reg16::Ax) & 0xff,
            7,
            "MOV AL,7 ran via the normal path"
        );
        assert_eq!(
            cpu.registers.eip, 0x1001,
            "eip advanced past the crossing MOV"
        );
    }
}

#[test]
fn straight_line_run_faults_on_cached_continuation_keeping_earlier_effects() {
    // A fault raised by a CACHED straight-line instruction running as a continuation
    // (run_one_cached) must route through the SAME tail the per-instruction path uses: a
    // delivered #DE (divide-by-zero) retargets CS:IP at the guest's own IVT handler, and the
    // earlier straight-line instruction's effects are kept. DIV is data-dependent, so it can
    // be cached with a good divisor and then fault on a later run with a zero divisor -
    // exactly the case where the faulting instruction is a valid cache hit (a delivered IVT
    // exception reloads CS and flushes the cache, so a register-input fault -- not a decode
    // change -- is the way to reach the cached-continuation path).
    //
    //   0x10: 40           INC AX     ; straight-line, runs before the DIV in the same run
    //   0x11: F6 F3        DIV BL     ; AX / BL ; #DE when BL = 0
    //   0x13: F4           HLT
    //
    // Code starts at 0x10 (not 0x00) so it does not overlap the real-mode IVT's vector-0 slot
    // (bytes 0..4), which this test populates with a trap handler address.
    const ORIGIN: usize = 0x10;
    let code = [
        0x40, // INC AX
        0xf6, 0xf3, // DIV BL
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&[], 0x1_0000);
    memory[ORIGIN..ORIGIN + code.len()].copy_from_slice(&code);
    memory[0..2].copy_from_slice(&DE_TRAP_IP.to_le_bytes());
    memory[2..4].copy_from_slice(&DE_TRAP_CS.to_le_bytes());
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_esp(0x2000);
    let mut bus = TestBus::with_memory(memory);

    // Warming pass with a good divisor: AX = 11, BL = 2. This caches both INC and DIV in the live
    // generation WITHOUT any fault (no CS reload, so the decode cache stays valid).
    cpu.registers.set_eax(11);
    cpu.write_reg16(Reg16::Bx, 0x0002);
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert!(
        cpu.decode_cache.get(ORIGIN as u32 + 1, false).is_some(),
        "DIV must be cached after the warming pass"
    );

    // Now poke the divisor to 0 and run from the top: INC is the run's first instruction, then the
    // CACHED DIV runs as a straight-line continuation and delivers #DE.
    cpu.registers.eip = ORIGIN as u32;
    cpu.registers.set_eax(10);
    cpu.write_reg16(Reg16::Bx, 0x0000);
    let outcome = cpu
        .run_straight_line(&mut bus, u64::MAX)
        .expect("the cached DIV continuation must deliver #DE, not error the run");
    assert!(!outcome.halted);
    assert_eq!(cpu.registers.cs().selector, DE_TRAP_CS);
    assert_eq!(cpu.registers.eip, u32::from(DE_TRAP_IP));
    // INC AX ran before the fault and its effect is kept (AX = 11).
    assert_eq!(cpu.read_reg16(Reg16::Ax), 11);
}

#[test]
fn straight_line_run_executes_cached_relative_jump_continuation() {
    // A cached relative JMP is safe to run as a continuation: it only changes EIP, and the next
    // continuation lookup uses that live target rather than falling through into skipped bytes.
    //
    //   0x00: 40           INC AX
    //   0x01: 40           INC AX
    //   0x02: EB 02        JMP +2 -> 0x06
    //   0x04: 40           INC AX   (skipped by the jump)
    //   0x05: 40           INC AX   (skipped)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0x40, // INC AX
        0xeb, 0x02, // JMP +2 -> 0x06
        0x40, // INC AX (skipped)
        0x40, // INC AX (skipped)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.registers.set_eax(0);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the HLT target is still a terminator");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);
    assert_eq!(
        cpu.registers.eip, 0x06,
        "the cached JMP ran and skipped to the HLT target"
    );
}

#[test]
fn straight_line_run_executes_cached_near_call_continuation() {
    // CALL near is a relative branch plus a normal stack push, and near RET is now a
    // continuable near transfer too, so the warm run chains CALL -> body -> RET -> return
    // site all the way to the HLT.
    //
    //   0x00: B8 01 00     MOV AX, 1
    //   0x03: E8 03 00     CALL 0x09        ; return address 0x06
    //   0x06: 40           INC AX           ; return site, reached through the chained RET
    //   0x07: F4           HLT
    //   0x08: 90           NOP
    //   0x09: 40           INC AX           ; subroutine
    //   0x0A: C3           RET              ; chained continuation
    let code = [
        0xb8, 0x01, 0x00, // MOV AX, 1
        0xe8, 0x03, 0x00, // CALL +3 -> 0x09
        0x40, // INC AX (return site)
        0xf4, // HLT
        0x90, // NOP
        0x40, // INC AX (subroutine)
        0xc3, // RET
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x01fe..0x0200].fill(0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the run stops AT the HLT terminator");
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        3,
        "subroutine and return site both ran"
    );
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0200,
        "RET released the return address"
    );
    assert_eq!(
        cpu.registers.eip, 0x07,
        "cached CALL chained through the RET back to the return site up to the HLT"
    );
}

#[test]
fn straight_line_run_chains_a_near_ret_procedure_in_one_run() {
    // The P1b chaining property: a warm CALL rel16 -> body -> near RET procedure executes as
    // ONE run (no brk[branch] break at the RET), proven via the run/break perf counters.
    //
    //   0x00: B9 02 00     MOV CX, 2
    //   0x03: E8 05 00     CALL 0x0B        ; return address 0x06
    //   0x06: 49           DEC CX
    //   0x07: 75 FA        JNZ 0x03         ; call again
    //   0x09: F4           HLT
    //   0x0A: 90           NOP
    //   0x0B: 40           INC AX           ; body
    //   0x0C: C3           RET
    let code = [
        0xb9, 0x02, 0x00, // MOV CX, 2
        0xe8, 0x05, 0x00, // CALL +5 -> 0x0B
        0x49, // DEC CX
        0x75, 0xfa, // JNZ -6 -> 0x03
        0xf4, // HLT
        0x90, // NOP
        0x40, // INC AX (body)
        0xc3, // RET
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Cx, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    // The run chains both CALL -> body -> RET round trips and stops only at the HLT
    // (HLT is Misc, still a terminator that runs on the next runner entry).
    assert!(
        !outcome.halted,
        "the run stops AT the HLT, which runs next entry"
    );
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2, "the body ran on both calls");
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0200,
        "both RETs released their frames"
    );
    assert_eq!(cpu.registers.eip, 0x09, "one run reached the HLT");
    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one runner entry covered the whole procedure"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "only the HLT terminator broke the run; the near RETs chained (eip pins that \
             the break was at 0x09, past both RETs)"
    );
    assert_eq!(p.brk_halt, 0, "HLT was not executed inside the run");
}

#[test]
fn straight_line_run_still_breaks_at_far_ret() {
    // The contrast case: far RET loads CS, so it stays a run terminator even warm.
    //
    //   0x00: 40           INC AX
    //   0x01: CB           RETF       ; stack target 0000:0006
    //   0x02: 40 40 40 90  (skipped)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0xcb, // RETF
        0x40, 0x40, 0x40, 0x90, // skipped
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 1024);
    memory[0x100..0x104].copy_from_slice(&[0x06, 0x00, 0x00, 0x00]); // 0000:0006
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x100..0x104].copy_from_slice(&[0x06, 0x00, 0x00, 0x00]);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "far RET must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached RETF"
    );
}

#[test]
fn straight_line_run_chains_rep_movs_mid_run() {
    // A REP MOVS mid-block runs as a continuation: the whole warm block (setup, the
    // atomic REP, and the instruction after it) is ONE runner entry ending at the HLT.
    //
    //   0x00: B9 03 00     MOV CX, 3
    //   0x03: BE 40 00     MOV SI, 0x40
    //   0x06: BF 60 00     MOV DI, 0x60
    //   0x09: F3 A4        REP MOVSB
    //   0x0B: 40           INC AX          ; still inside the same run
    //   0x0C: F4           HLT
    let code = [
        0xb9, 0x03, 0x00, // MOV CX, 3
        0xbe, 0x40, 0x00, // MOV SI, 0x40
        0xbf, 0x60, 0x00, // MOV DI, 0x60
        0xf3, 0xa4, // REP MOVSB
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 1024);
    memory[0x40..0x43].copy_from_slice(b"abc");
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.memory[0x60..0x63].fill(0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Cx, 0);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted, "the run stops AT the HLT terminator");
    assert_eq!(&bus.memory[0x60..0x63], b"abc", "the REP MOVS copied");
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0, "the repeat ran to exhaustion");
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1, "the post-REP INC chained");
    assert_eq!(cpu.registers.eip, 0x0c, "one run reached the HLT");
    let p = cpu.perf_counters();
    assert_eq!(
        p.straight_line_runs, 1,
        "one runner entry covered the block"
    );
    assert_eq!(
        p.brk_decode_or_branch, 1,
        "only the HLT terminator broke the run; the REP MOVS chained"
    );
}

#[test]
fn straight_line_run_still_breaks_at_string_port_io() {
    // OUTSB (0x6E, Misc group) touches a port, so it must never run as a continuation
    // even warm: the run breaks at the gate and OUTSB runs on the next runner entry.
    //
    //   0x00: 40           INC AX
    //   0x01: 6E           OUTSB      ; must not run as a continuation
    //   0x02: F4           HLT
    let code = [
        0x40, // INC AX
        0x6e, // OUTSB
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    bus.io_touched = false; // clear the warm drive's port-touch step-break latch
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "OUTSB must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached OUTSB"
    );
}

#[test]
fn straight_line_run_continues_push_rm_but_breaks_far_indirect() {
    // The 0xFF split: /6 PUSH r/m is a plain fall-through form and chains; /3 far
    // indirect CALL loads CS and stays a terminator.
    //
    //   0x00: 40           INC AX
    //   0x01: FF 36 40 00  PUSH word [0x0040]
    //   0x05: 40           INC AX
    //   0x06: F4           HLT
    let push_code = [
        0x40, // INC AX
        0xff, 0x36, 0x40, 0x00, // PUSH word [0x0040]
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&push_code, 1024);
    memory[0x40..0x42].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        2,
        "both INCs chained past the PUSH"
    );
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x01fe], bus.memory[0x01ff]]),
        0xbeef,
        "PUSH r/m ran as a continuation"
    );
    assert_eq!(cpu.registers.eip, 0x06, "one run reached the HLT");
    assert_eq!(cpu.perf_counters().straight_line_runs, 1);
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "only the HLT broke"
    );

    //   0x00: 40           INC AX
    //   0x01: FF 1E 40 00  CALL FAR [0x0040]   ; m16:16 -> 0000:0008
    //   0x05: 40 40 90     (not reached in the warm run)
    //   0x08: F4           HLT
    let far_code = [
        0x40, // INC AX
        0xff, 0x1e, 0x40, 0x00, // CALL FAR [0x0040]
        0x40, 0x40, 0x90, // filler
        0xf4, // HLT
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&far_code, 1024);
    memory[0x40..0x44].copy_from_slice(&[0x08, 0x00, 0x00, 0x00]); // 0000:0008
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);
    drive_straight_line_runs(&mut cpu, &mut bus);

    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.write_reg16(Reg16::Sp, 0x0200);
    cpu.halted = false;
    cpu.reset_perf_counters();
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 1);
    assert_eq!(
        cpu.registers.eip, 0x01,
        "far indirect CALL must not run as a continuation"
    );
    assert_eq!(
        cpu.perf_counters().brk_decode_or_branch,
        1,
        "the warm run broke at the cached 0xFF /3"
    );
}

#[test]
fn straight_line_run_never_executes_an_int_after_a_taken_branch() {
    // Regression guard against the "recompiler executes non-executed code" claim: a
    // side-effecting instruction (INT 0x13) sitting in the contiguous bytes AFTER a taken
    // branch must NEVER be dispatched, even after the decode cache is warm. The cached JMP here
    // makes EIP skip the INT entirely, so the executed-INT trace must stay empty for vector 0x13.
    //
    //   0x00: 40           INC AX
    //   0x01: 40           INC AX
    //   0x02: EB 02        JMP +2 -> 0x06        (taken branch over the INT)
    //   0x04: CD 13        INT 0x13              (contiguous bytes; must NEVER run)
    //   0x06: F4           HLT
    let code = [
        0x40, // INC AX
        0x40, // INC AX
        0xeb, 0x02, // JMP +2 -> 0x06 (skips the INT 0x13)
        0xcd, 0x13, // INT 0x13 (must never execute)
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);

    // First drive: warms the decode cache (the INC/INC/JMP block becomes a cached run) and runs
    // to the HLT. A warm cache is exactly the condition under which an over-read of trailing
    // bytes would surface, so this is the case the claim must be tested against.
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert_eq!(
        cpu.read_reg16(Reg16::Ax),
        2,
        "only the two pre-JMP INCs ran"
    );
    assert_eq!(cpu.registers.eip, 0x07, "control reached the HLT at 0x06");

    // Re-arm and drive again from the top with the cache now hot, to be sure a cached relative
    // branch continuation still targets the HLT rather than over-reading into the INT bytes.
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0);
    cpu.halted = false;
    drive_straight_line_runs(&mut cpu, &mut bus);
    assert_eq!(cpu.read_reg16(Reg16::Ax), 2);

    // The decisive assertion: across both drives, NO software interrupt was ever acknowledged
    // for vector 0x13. `software_interrupt` is the single dispatch point for an executed INT n,
    // and it always calls `bus.interrupt_acknowledge(vector, ..)`, which the TestBus records as
    // an InterruptAcknowledge cycle. An empty result proves the post-branch INT bytes are inert.
    let executed_int13 = bus
        .trace
        .cycles()
        .iter()
        .filter(|c| c.kind == BusAccessKind::InterruptAcknowledge && c.address == 0x13)
        .count();
    assert_eq!(
        executed_int13, 0,
        "the straight-line executor dispatched INT 0x13 from bytes after a taken branch; \
             this would be a genuine over-read of non-executed code"
    );
}

#[test]
fn straight_line_run_ends_on_port_io_step_break() {
    // A port access (OUT) touches time-dependent device state, so the old per-instruction machine
    // loop ended the step immediately after it (io_touched). The executor must do the same via
    // bus.requires_step_break(): an OUT as the run's FIRST instruction ends the run after that one
    // instruction, so the following straight-line INC does NOT run in the same call. Without the
    // step-break the executor would keep going and the device boundary would drift.
    //
    //   0x00: E6 80   OUT 0x80, AL   ; PortIo -> sets io_touched
    //   0x02: 40      INC AX         ; must NOT run in the same run as the OUT
    //   0x03: F4      HLT
    let code = [
        0xe6, 0x80, // OUT 0x80, AL
        0x40, // INC AX
        0xf4, // HLT
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 1024);
    let mut bus = TestBus::with_memory(memory);
    let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
    assert!(!outcome.halted);
    // eip advanced past only the OUT (2 bytes); the run broke before the INC.
    assert_eq!(cpu.registers.eip, 0x02);
    // The INC did not run in this call, so AX is unchanged.
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0);
    assert!(bus.io_touched, "the OUT must have touched device I/O");
}

/// Shared seed for the seam differential / golden batteries below: a fixed real-mode register
/// set plus a known word at [0x20], so each instruction has stable inputs.
fn seam_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0102);
    cpu.write_reg16(Reg16::Bx, 0x0010);
    cpu.write_reg16(Reg16::Cx, 0x0304);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.write_reg16(Reg16::Bp, 0x0010);
}

fn seam_fetch_count(bus: &TestBus) -> usize {
    bus.trace
        .cycles()
        .iter()
        .filter(|c| c.kind == BusAccessKind::InstructionPrefetch)
        .count()
}

#[test]
fn seam_matches_fused_path_across_addressing_forms() {
    // Historically this diffed a *still-on-Fallback* memory-read opcode through cycle()
    // (decode/execute split) against execute_instruction_legacy (fused) to guard the seam.
    // After task A14 there is no longer any IMPLEMENTED opcode on Fallback to diff this way —
    // every implemented opcode is converted to the split, so the fused executor for each was
    // deleted. `inc word [bx]` (0xff), `test [bx],cx` (0x85), then `xlat` (0xd7) each served as
    // the exemplar in turn and were converted away (`ControlFlow`/`FlagsMisc`/`Misc`). The seam's
    // memory-read + single-fetch-charge behaviour is now covered by the per-group golden
    // batteries (which assert eip, the memory write/read, AND `seam_fetch_count` == golden). Run
    // XLAT — the last memory-read exemplar, now `DecodeGroup::Misc` — through the split and assert
    // it both reads the right table byte AND charges each instruction-fetch byte exactly once.
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0xd7; // XLAT
    mem[0x12] = 0xab; // the XLAT lookup result planted at [BX+AL]=0x12 (BX=0x10, AL=0x02)

    let mut split = Cpu386::default();
    seam_seed(&mut split);
    let mut sbus = TestBus::with_memory(mem);
    exec_one_split(&mut split, &mut sbus).unwrap();

    // AL = [DS:BX+AL] = mem[0x12] = 0xab; the rest of AX (AH=0x01) is unchanged.
    assert_eq!(split.read_reg16(Reg16::Ax), 0x01ab, "xlat result");
    assert_eq!(split.registers.eip, 0x1, "eip past the 1-byte opcode");
    // Clock-neutrality guard: 1 opcode-prefetch peek + 1 opcode byte = 2 instruction fetches;
    // the data read of the table byte is a DataRead, not an InstructionPrefetch. A decode/execute
    // double-charge of the opcode would push this past 2.
    assert_eq!(
        seam_fetch_count(&sbus),
        2,
        "the seam must charge each instruction-fetch byte exactly once"
    );
}

/// One golden end-state for an ALU case run from `seam_seed`: the opcode bytes plus the
/// expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory writes, and
/// InstructionPrefetch fetch count. Shared between the assertion test and the regen helper.
struct AluGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// The ALU differential battery: every op/form/addressing-mode case plus its golden end-state.
///
/// HOW TO CAPTURE / REGENERATE GOLDENS (read before editing any `gpr`/`eflags`/`deltas`/`fetch`
/// below, and follow this same recipe for every future group-conversion task):
///   1. The goldens are captured from the PRIOR fused reference (`execute_instruction_legacy`),
///      NOT from the new split path. Capturing from the split would be tautological — it would
///      assert the code matches itself and catch nothing.
///   2. Run `cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture` while
///      the group's fused arm still exists, then paste the printed literals here. For a new
///      group, capture BEFORE you delete its fused arm from `dispatch_opcode`.
///   3. For THIS (ALU) group the fused arm is already gone on `perf-decode-cache`, so the regen
///      helper must be run from the pre-split base commit (332be72): `git stash`, check out the
///      base, run the command, paste, then return. (These goldens were captured exactly so.)
///   4. Never hand-edit a golden to make a failing test pass — re-capture from the reference.
fn alu_golden_cases() -> &'static [AluGolden] {
    &[
        // Forms 0-3 (r/m,reg and reg,r/m, byte and word), several addressing modes.
        AluGolden {
            name: "add ax,bx",
            code: &[0x01, 0xd8],
            gpr: [274, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "add [bx+si],ax",
            code: &[0x01, 0x00],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(24, 2), (25, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "add [bp+di+4],cx",
            code: &[0x01, 0x4b, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x03,
            deltas: &[(44, 4), (45, 3)],
            fetch: 4,
        },
        AluGolden {
            name: "add [0x20],dx",
            code: &[0x01, 0x16, 0x20, 0x00],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x04,
            deltas: &[(32, 23), (33, 22)],
            fetch: 5,
        },
        AluGolden {
            name: "add [si],al(byte)",
            code: &[0x00, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(8, 2)],
            fetch: 3,
        },
        // Every ALU op through word r/m,reg (form 1) with a memory operand: op-by-op coverage.
        AluGolden {
            name: "add [bx],ax(form1)",
            code: &[0x01, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "or [bx],ax(form1)",
            code: &[0x09, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "adc [bx],ax(form1)",
            code: &[0x11, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "sbb [bx],ax(form1)",
            code: &[0x19, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[(16, 254), (17, 254)],
            fetch: 3,
        },
        AluGolden {
            name: "and [bx],ax(form1)",
            code: &[0x21, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "sub [bx],ax(form1)",
            code: &[0x29, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[(16, 254), (17, 254)],
            fetch: 3,
        },
        AluGolden {
            name: "xor [bx],ax(form1)",
            code: &[0x31, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(16, 2), (17, 1)],
            fetch: 3,
        },
        AluGolden {
            name: "cmp [bx],ax(form1)",
            code: &[0x39, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        // reg,r/m direction (form 3, word; writes a register) and byte directions (forms 0/2).
        AluGolden {
            name: "or cx,[bx+si]",
            code: &[0x0b, 0x08],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "and dx,[di]",
            code: &[0x23, 0x15],
            gpr: [258, 772, 0, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "adc al,[bx](byte form2)",
            code: &[0x12, 0x07],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "xor [si],bl(byte form0)",
            code: &[0x30, 0x1c],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(8, 16)],
            fetch: 3,
        },
        // Immediate accumulator forms: byte AL,imm8 (form 4) and word AX,imm16 (form 5).
        AluGolden {
            name: "add al,imm8(form4)",
            code: &[0x04, 0x7f],
            gpr: [385, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x896,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "or al,imm8(form4)",
            code: &[0x0c, 0xaa],
            gpr: [426, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x86,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "cmp al,imm8(form4)",
            code: &[0x3c, 0x05],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x02,
            deltas: &[],
            fetch: 3,
        },
        AluGolden {
            name: "add ax,imm16(form5)",
            code: &[0x05, 0x34, 0x12],
            gpr: [4918, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x06,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        AluGolden {
            name: "sub ax,imm16(form5)",
            code: &[0x2d, 0x34, 0x12],
            gpr: [61134, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        AluGolden {
            name: "cmp ax,imm16(form5)",
            code: &[0x3d, 0x02, 0x01],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
        // Remaining addressing forms carried over from the original battery.
        AluGolden {
            name: "sub [bp+2],ax",
            code: &[0x29, 0x46, 0x02],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x03,
            deltas: &[(18, 254), (19, 254)],
            fetch: 4,
        },
        AluGolden {
            name: "xor [di],bx",
            code: &[0x31, 0x1d],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x02,
            eip: 0x02,
            deltas: &[(24, 16)],
            fetch: 3,
        },
        AluGolden {
            name: "cmp [bx+4],dx",
            code: &[0x39, 0x57, 0x04],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x97,
            eip: 0x03,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn alu_split_matches_golden_across_ops() {
    // The whole ALU block (ADD/OR/ADC/SBB/AND/SUB/XOR/CMP) is converted to the decode/execute
    // split, so it can no longer be diffed against a fused executor (that path was deleted to
    // keep a single ALU implementation). Instead, run each op/form through cycle() and assert
    // the architectural end-state against goldens captured from the pre-split fused path
    // (commit 332be72; see `alu_golden_cases` for the capture recipe). This exercises decode's
    // ModRM/immediate parsing, the executor's operand wiring + write-back gating, the EA
    // recompute, and the once-only instruction-fetch charge.
    for g in alu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut split = Cpu386::default();
        seam_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate the `alu_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default (it only prints; it asserts nothing). This is the copy-paste template for every
/// future group-conversion task: drive each case through `execute_instruction_legacy` (the
/// fused path) and print a ready-to-paste golden literal, so the goldens come from the
/// reference implementation rather than from the split path they guard (which would be
/// tautological).
///
/// Run it WHILE the group's fused arm still exists:
///   cargo test -p izarravm-cpu --lib regen_alu_goldens -- --ignored --nocapture
/// For the ALU group specifically the fused arm is already deleted on this branch, so this must
/// be run from the pre-split base commit (332be72) — see the recipe on `alu_golden_cases`. A
/// case whose opcode the current fused path can no longer execute prints a TODO marker instead
/// of a wrong literal, so a stale run can never silently bake bad goldens.
///
/// The printed `code` bytes are decimal (e.g. `&[1, 216]`); that compiles identically to the
/// hex source form, so paste the numeric result fields and keep your hex encoding if preferred.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_alu_goldens() {
    for g in alu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        // Stage A removed the in-tree fused reference (`execute_instruction_legacy`), so this
        // checkout's regen captures from the production split instead — which is tautological for
        // catching split bugs (the goldens it prints are exactly what the split now produces).
        // Only use an in-checkout regen run to RE-derive goldens after an intentional behavior
        // change; to capture an INDEPENDENT reference, run this test from a pre-Stage-A worktree
        // (see the recipe on the cases fn). A case the split can't execute prints a TODO marker
        // rather than a wrong literal.
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: opcode not executable here; run from a pre-Stage-A worktree",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            AluGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// One golden end-state for a data-movement case, captured the same way as `AluGolden`: opcode
/// bytes plus expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, (offset,value) memory
/// writes, and InstructionPrefetch fetch count. Data-movement ops do not touch flags, so the
/// eflags field just confirms that (it should equal the seed's `0x02`).
struct DataMoveGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// The data-movement differential battery: MOV/LEA/XCHG across forms and addressing modes, plus
/// the moffs / immediate / Sreg variants, each with its golden end-state. Captured from the
/// PRIOR fused reference (`execute_instruction_legacy` -> `dispatch_opcode`) via
/// `regen_datamove_goldens`; see `alu_golden_cases` for the full capture recipe (the goldens
/// must come from the reference path, never from the split path they guard). The two-byte
/// MOVZX/MOVSX forms — also in `DecodeGroup::DataMove` — have their own battery
/// (`movzx_movsx_golden_cases`), so they are absent here.
fn datamove_golden_cases() -> &'static [DataMoveGolden] {
    &[
        // MOV r/m<->reg, byte and word, register and memory r/m.
        DataMoveGolden {
            name: "mov [bx],cx",
            code: &[137, 15],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(16, 4), (17, 3)],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov [bp+si+4],al(byte)",
            code: &[136, 66, 4],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(28, 2)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov dx,bx(reg)",
            code: &[137, 218],
            gpr: [258, 772, 16, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov cx,[0x20]",
            code: &[139, 14, 32, 0],
            gpr: [258, 4369, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        DataMoveGolden {
            name: "mov al,[bx](byte)",
            code: &[138, 7],
            gpr: [256, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // MOV r/m,Sreg and MOV Sreg,r/m (load ES, leaves the addressing segments untouched).
        DataMoveGolden {
            name: "mov [bx],es",
            code: &[140, 7],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov es,[0x20]",
            code: &[142, 6, 32, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // LEA: effective address into the register, disp+index and direct-disp forms.
        DataMoveGolden {
            name: "lea ax,[bx+si+3]",
            code: &[141, 64, 3],
            gpr: [27, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "lea dx,[0x20]",
            code: &[141, 22, 32, 0],
            gpr: [258, 772, 32, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // MOV (E)AX<->moffs, byte and word, read and write.
        DataMoveGolden {
            name: "mov al,[moffs8 0x20]",
            code: &[160, 32, 0],
            gpr: [273, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov ax,[moffs 0x20]",
            code: &[161, 32, 0],
            gpr: [4369, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov [moffs8 0x30],al",
            code: &[162, 48, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(48, 2)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov [moffs 0x30],ax",
            code: &[163, 48, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(48, 2), (49, 1)],
            fetch: 4,
        },
        // MOV r,imm (byte and word).
        DataMoveGolden {
            name: "mov bl,0x7f",
            code: &[179, 127],
            gpr: [258, 772, 1286, 127, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "mov si,0x1234",
            code: &[190, 52, 18],
            gpr: [258, 772, 1286, 16, 0, 16, 4660, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOV r/m,imm (group 11), register and memory.
        DataMoveGolden {
            name: "mov byte [bx],0x55",
            code: &[198, 7, 85],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(16, 85)],
            fetch: 4,
        },
        DataMoveGolden {
            name: "mov word [bx],0xbeef",
            code: &[199, 7, 239, 190],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(16, 239), (17, 190)],
            fetch: 5,
        },
        DataMoveGolden {
            name: "mov dx,0xabcd(grp11 reg)",
            code: &[199, 194, 205, 171],
            gpr: [258, 772, 43981, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // XCHG r/m,reg (byte and word, register and memory) and XCHG (E)AX,reg + NOP.
        DataMoveGolden {
            name: "xchg [bx],cx",
            code: &[135, 15],
            gpr: [258, 0, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(16, 4), (17, 3)],
            fetch: 3,
        },
        DataMoveGolden {
            name: "xchg dl,bl(byte reg)",
            code: &[134, 211],
            gpr: [258, 772, 1296, 6, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        DataMoveGolden {
            name: "xchg ax,cx",
            code: &[145],
            gpr: [772, 258, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        DataMoveGolden {
            name: "nop",
            code: &[144],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
    ]
}

#[test]
fn datamove_split_matches_golden_across_ops() {
    // The single-byte data-movement block (MOV/LEA/XCHG and their immediate/moffs/Sreg forms)
    // is converted to the decode/execute split, so it can no longer be diffed against a fused
    // executor (that path was deleted to keep a single implementation). Instead, run each form
    // through cycle() and assert the architectural end-state against goldens captured from the
    // pre-split fused path (see `datamove_golden_cases` for the capture recipe). This exercises
    // decode's ModRM/immediate/moffs parsing, the executor's operand wiring, the EA recompute,
    // and the once-only instruction-fetch charge.
    for g in datamove_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut split = Cpu386::default();
        seam_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate the `datamove_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default (it only prints). Mirror of `regen_alu_goldens`: drive each case through
/// `execute_instruction_legacy` (the fused path) and print a ready-to-paste literal, so the
/// goldens come from the reference rather than the split path they guard.
///
/// Run it WHILE the group's fused arms still exist in `dispatch_opcode`:
///   cargo test -p izarravm-cpu --lib regen_datamove_goldens -- --ignored --nocapture
/// then paste the output over `datamove_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_datamove_goldens() {
    for g in datamove_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        mem[0x20..0x22].copy_from_slice(&0x1111u16.to_le_bytes());
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            DataMoveGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// One golden end-state for a MOVZX/MOVSX case run from `movzx_seed` (a real-mode register set
/// with sentinel bytes/words in memory). The opcode bytes plus expected end gpr, eflags
/// (MOVZX/MOVSX never touch flags, so this must stay the seed's `0x02`), eip, memory writes
/// (always empty — these are pure loads), and InstructionPrefetch fetch count. Captured from the
/// PRIOR fused reference via `regen_movzx_movsx_goldens`; see `alu_golden_cases` for the recipe.
struct MovzxMovsxGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the MOVZX/MOVSX battery. Same register set as `seam_seed`, but it also plants
/// sentinels so the byte/word, sign/zero, and EA-recompute cases have stable, sign-bit-set
/// sources: byte 0x80 at [0x10] (= [BX]), word 0x8081 at [0x18] (= [BX+SI], BX=0x10 + SI=0x08),
/// and word 0xBEEF at [0x20] (the direct-disp source). The 0x80/0x8081/0xBEEF high bits make
/// zero- vs sign-extension visibly different.
fn movzx_seed(cpu: &mut Cpu386, mem: &mut [u8]) {
    seam_seed(cpu);
    mem[0x10] = 0x80;
    mem[0x18..0x1a].copy_from_slice(&0x8081u16.to_le_bytes());
    mem[0x20..0x22].copy_from_slice(&0xBEEFu16.to_le_bytes());
}

/// The MOVZX/MOVSX differential battery: 0F B6/B7 (zero-extend byte/word) and 0F BE/BF
/// (sign-extend byte/word), each in a register form and a memory form, plus an EA-recompute
/// case ([BX+SI], resolved against the live registers in the executor). Goldens captured from
/// the fused reference (`execute_instruction_legacy`); never edit by hand — re-run the regen.
fn movzx_movsx_golden_cases() -> &'static [MovzxMovsxGolden] {
    &[
        // MOVZX r16, r/m8 (0F B6): zero-extend a byte. BL = low byte of BX(0x10) = 0x10.
        MovzxMovsxGolden {
            name: "movzx ax, bl(reg)",
            code: &[15, 182, 195],
            gpr: [16, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // byte [BX] = [0x10] = 0x80, zero-extended to 0x0080 (= 128).
        MovzxMovsxGolden {
            name: "movzx ax, [bx](byte, sign bit set)",
            code: &[15, 182, 7],
            gpr: [128, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOVZX r16, r/m16 (0F B7): word [0x20] = 0xBEEF, zero-extended (= 48879).
        MovzxMovsxGolden {
            name: "movzx cx, [0x20](word)",
            code: &[15, 183, 14, 32, 0],
            gpr: [258, 48879, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // MOVSX r16, r/m8 (0F BE): byte [BX] = 0x80, sign-extended to 0xFF80 (= 65408).
        MovzxMovsxGolden {
            name: "movsx dx, [bx](byte, sign bit set)",
            code: &[15, 190, 23],
            gpr: [258, 772, 65408, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // DL = low byte of DX(0x0506) = 0x06, positive, sign-extends to 0x0006 (= 6).
        MovzxMovsxGolden {
            name: "movsx ax, dl(reg, positive byte)",
            code: &[15, 190, 194],
            gpr: [6, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // MOVSX r16, r/m16 (0F BF), EA recomputed from live BX+SI = 0x18; word [0x18] = 0x8081,
        // sign-extended stays 0x8081 at 16 bits (= 32897).
        MovzxMovsxGolden {
            name: "movsx ax, [bx+si](word, sign bit set)",
            code: &[15, 191, 0],
            gpr: [32897, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn movzx_movsx_split_matches_golden() {
    // MOVZX/MOVSX (0F B6/B7/BE/BF) are converted to the split, so they can no longer be diffed
    // against a fused executor (that arm was deleted). Run each through cycle() and assert the
    // architectural end-state against goldens captured from the pre-split fused path. Covers
    // byte and word sources, zero vs sign extend, reg and mem operands, and an EA-recompute
    // case. MOVZX/MOVSX do not modify flags, so eflags must stay the seed value.
    for g in movzx_movsx_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let mut split = Cpu386::default();
        movzx_seed(&mut split, &mut mem);
        let initial = mem.clone();
        let mut sbus = TestBus::with_memory(mem);
        split.cycle(&mut sbus).expect("movzx/movsx must execute");

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(
            split.eflags(),
            g.eflags,
            "eflags mismatch for {} (MOVZX/MOVSX must not touch flags)",
            g.name
        );
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate the `movzx_movsx_golden_cases` literals from the PRIOR fused reference. Ignored by
/// default. Mirror of `regen_datamove_goldens`; run WHILE the MOVZX/MOVSX arms still exist in
/// `execute_two_byte`:
///   cargo test -p izarravm-cpu --lib regen_movzx_movsx_goldens -- --ignored --nocapture
/// then paste the output over `movzx_movsx_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_movzx_movsx_goldens() {
    for g in movzx_movsx_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let mut fused = Cpu386::default();
        movzx_seed(&mut fused, &mut mem);
        let initial = mem.clone();
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting the fused arms",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            MovzxMovsxGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

#[test]
fn group11_mov_rm_imm_with_nonzero_reg_faults_without_consuming_the_immediate() {
    // C6 /1 is an undefined group-11 encoding (only reg=000 is MOV r/m,imm). This is the one
    // data-move path the goldens can't cover: decode DEFERS parsing the operand/immediate when
    // reg != 0 and the executor re-raises the error. Drive it through the split (which returns
    // the raw fault without eip rewind) and assert two things:
    //   1. the fault is a deliverable #UD (vector 6, no error code), and
    //   2. eip advanced to exactly 2 (opcode + ModRM) — proving decode did NOT over-consume the
    //      trailing imm8 (0x55) on the fault path, so the bytes charged match the fused handler.
    let (mut cpu, memory) = real_mode_cpu(&[0xc6, 0xc9, 0x55], 0x20);
    let mut bus = TestBus::with_memory(memory);

    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "{fault:?}"
    );
    assert_eq!(
        cpu.registers.eip, 2,
        "decode must stop after the ModRM on the fault path (imm8 not consumed)"
    );
}

#[test]
fn decode_then_execute_matches_golden_for_add_rm_reg() {
    // 01 D8 = ADD AX, BX (ALU form 1, op=0, modrm mode=3 rm=0 reg=3). The decode +
    // execute_decoded path must produce the architectural ADD result. (Once the ALU block was
    // fully converted to the split, the former fused executor was deleted, so this asserts the
    // known-correct end-state directly rather than diffing against a removed reference path.)
    let code = [0x01, 0xd8];

    let (mut split, mem) = real_mode_cpu(&code, 0x10);
    split.write_reg16(Reg16::Ax, 0x1234);
    split.write_reg16(Reg16::Bx, 0x1111);
    let mut split_bus = TestBus::with_memory(mem);
    split.begin_instruction();
    let insn = split.decode(&mut split_bus).unwrap();
    assert_eq!(insn.opcode, 0x01);
    assert_eq!(insn.operand, Some(DecodedOperand::Reg(0))); // r/m = AX
    let split_outcome = split.execute_decoded(&insn, &mut split_bus).unwrap();

    // 0x1234 + 0x1111 = 0x2345: no carry/zero/sign/overflow/aux, low byte 0x45 has odd parity
    // (PF clear), so only the always-set reserved bit 1 remains.
    assert_eq!(split.read_reg16(Reg16::Ax), 0x2345);
    assert_eq!(split.read_reg16(Reg16::Bx), 0x1111); // source untouched
    assert_eq!(split.eflags(), 0x02);
    assert_eq!(split.registers.eip, 0x02);
    assert_eq!(split_outcome.core_clocks, 2);
}

#[test]
fn decoded_add_rm_reg_recomputes_ea_from_live_registers() {
    // 01 07 = ADD [BX], AX (modrm mode=0 rm=7 -> [BX]). Decode once, then change BX before
    // executing: the addressing-mode descriptor must resolve against the *new* BX, proving
    // the decoded form stores a descriptor and not a baked-in offset.
    let code = [0x01, 0x07];
    let (mut cpu, mut mem) = real_mode_cpu(&code, 0x40);
    // Seed both candidate target words.
    mem[0x20..0x22].copy_from_slice(&0x0001u16.to_le_bytes());
    mem[0x30..0x32].copy_from_slice(&0x0001u16.to_le_bytes());
    let mut bus = TestBus::with_memory(mem);
    cpu.write_reg16(Reg16::Ax, 0x0010);
    cpu.write_reg16(Reg16::Bx, 0x0020);

    cpu.begin_instruction();
    let insn = cpu.decode(&mut bus).unwrap();
    // The descriptor must name BX (register 3) as its base, not a resolved offset.
    match insn.operand {
        Some(DecodedOperand::Mem(addr)) => {
            assert_eq!(addr.base, Some(3));
            assert_eq!(addr.index, None);
            assert_eq!(addr.disp, 0);
        }
        other => panic!("expected a memory operand, got {other:?}"),
    }

    // Move the pointer before executing.
    cpu.write_reg16(Reg16::Bx, 0x0030);
    cpu.execute_decoded(&insn, &mut bus).unwrap();

    assert_eq!(bus.memory[0x20], 0x01, "old target must be untouched");
    assert_eq!(bus.memory[0x30], 0x11, "new target (BX=0x30) gets AX added");
}

#[test]
fn alu_split_recomputes_effective_address() {
    // 00 07 = ADD [BX], AL (ALU form 0, op=0). Decode once, then execute against two different
    // BX values: each execution must resolve [BX] against the *current* BX and update the byte
    // there, proving the generalized ALU split recomputes the effective address every run.
    let code = [0x00, 0x07];
    let (mut cpu, mut mem) = real_mode_cpu(&code, 0x60);
    mem[0x40] = 0x01;
    mem[0x50] = 0x02;
    let mut bus = TestBus::with_memory(mem);
    cpu.write_reg16(Reg16::Ax, 0x0010); // AL = 0x10, AH = 0

    cpu.begin_instruction();
    let insn = cpu.decode(&mut bus).unwrap();

    // First run with BX = 0x40: the byte at [0x40] gains AL.
    cpu.write_reg16(Reg16::Bx, 0x0040);
    cpu.execute_decoded(&insn, &mut bus).unwrap();
    assert_eq!(bus.memory[0x40], 0x11, "[BX=0x40] must get AL added");
    assert_eq!(bus.memory[0x50], 0x02, "[0x50] untouched on the first run");

    // Re-execute the SAME decoded instruction with BX = 0x50: the EA must follow BX.
    cpu.write_reg16(Reg16::Bx, 0x0050);
    cpu.execute_decoded(&insn, &mut bus).unwrap();
    assert_eq!(bus.memory[0x40], 0x11, "[0x40] untouched on the second run");
    assert_eq!(bus.memory[0x50], 0x12, "[BX=0x50] must get AL added");
}

#[test]
fn self_modified_opcode_beyond_prefetch_window_is_seen() {
    let mut code = vec![0x90; 0x40]; // nop sled
    code[0..5].copy_from_slice(&[0xc6, 0x06, 0x21, 0x00, 0xf4]); // mov byte [0021h],hlt
    code[0x21] = 0x90; // replaced before execution reaches it
    code[0x22..0x24].copy_from_slice(&[0xeb, 0xfe]); // stale path would loop here
    let (mut cpu, memory) = real_mode_cpu(&code, 0x40);
    let mut bus = TestBus::with_memory(memory);

    let mut halted = false;
    for _ in 0..40 {
        halted = cpu.cycle(&mut bus).unwrap().halted;
        if halted {
            break;
        }
    }

    assert!(halted, "modified HLT at 0021h must execute");
    assert_eq!(cpu.registers.eip, 0x22);
}

#[test]
fn int3_traps_to_vector_3() {
    // 0xCC. IVT[3] (linear 12) -> CS:IP = 0000:0100.
    let (mut cpu, mut memory) = real_mode_cpu(&[0xcc], 0x200);
    memory[12..14].copy_from_slice(&0x0100u16.to_le_bytes());
    memory[14..16].copy_from_slice(&0x0000u16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0200);
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.eip, 0x0100);
    assert_eq!(cpu.registers.cs().selector, 0);
    // flags, CS, return-IP(=1) were pushed: SP fell by 6, return IP word is 1.
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01fa);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x1fa], bus.memory[0x1fb]]),
        1
    );
}

#[test]
fn into_traps_only_when_overflow_set() {
    // 0xCE with OF=1 traps to vector 4 (IVT[4] linear 16 -> 0000:0200).
    let (mut cpu, mut memory) = real_mode_cpu(&[0xce], 0x300);
    memory[16..18].copy_from_slice(&0x0200u16.to_le_bytes());
    cpu.write_reg16(Reg16::Sp, 0x0280);
    cpu.set_flag(FLAG_OF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 0x0200, "OF set: INTO must trap");

    // OF=0: INTO is a no-op, just advances past the one byte.
    let (mut cpu, memory) = real_mode_cpu(&[0xce], 0x40);
    cpu.set_flag(FLAG_OF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 1, "OF clear: INTO must fall through");
}

#[test]
fn word_in_out_use_word_width() {
    // IN AX, DX (0xED): word port read lands in AX (TestBus returns 0).
    let (mut cpu, memory) = real_mode_cpu(&[0xed], 0x10);
    cpu.write_reg16(Reg16::Ax, 0xffff);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoRead
                && c.width == BusWidth::Word
                && c.address == 0x03f8)
    );

    // OUT DX, AX (0xEF): word port write at DX.
    let (mut cpu, memory) = real_mode_cpu(&[0xef], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite
                && c.width == BusWidth::Word
                && c.address == 0x03f8)
    );
}

#[test]
fn push_imm8_sign_extends_to_word() {
    // 0x6A 0x80 -> push 0xFF80 onto a 16-bit stack.
    let (mut cpu, memory) = real_mode_cpu(&[0x6a, 0x80], 0x120);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fe);
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xff80
    );
}

#[test]
fn imul_imm8_sign_extended() {
    // IMUL AX, AX, -1  (0x6B 0xC0 0xFF): 2 * -1 = -2, fits, CF/OF clear.
    let (mut cpu, memory) = real_mode_cpu(&[0x6b, 0xc0, 0xff], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x0002);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xfffe);
    assert!(!cpu.flag(FLAG_CF) && !cpu.flag(FLAG_OF));
}

#[test]
fn imul_imm16_overflow_sets_carry_and_overflow() {
    // IMUL AX, AX, 0x0004 (0x69 0xC0 0x04 0x00) with AX=0x4000 -> 0x10000, truncates.
    let (mut cpu, memory) = real_mode_cpu(&[0x69, 0xc0, 0x04, 0x00], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x4000);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0000);
    assert!(cpu.flag(FLAG_CF) && cpu.flag(FLAG_OF));
}

#[test]
fn enter_level_zero_builds_frame() {
    // ENTER 4, 0 (0xC8 0x04 0x00 0x00): push BP, BP=SP, SP-=4.
    let (mut cpu, memory) = real_mode_cpu(&[0xc8, 0x04, 0x00, 0x00], 0x120);
    cpu.write_reg16(Reg16::Bp, 0xbbbb);
    cpu.write_reg16(Reg16::Sp, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Bp),
        0x00fe,
        "BP = frame after PUSH BP"
    );
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x00fa, "SP -= alloc");
    assert_eq!(
        u16::from_le_bytes([bus.memory[0xfe], bus.memory[0xff]]),
        0xbbbb
    );
}

#[test]
fn salc_sets_al_from_carry() {
    // 0xD6 with CF=1 -> AL=0xFF (AH preserved).
    let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.set_flag(FLAG_CF, true);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x12ff);

    // CF=0 -> AL=0x00.
    let (mut cpu, memory) = real_mode_cpu(&[0xd6], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x1234);
    cpu.set_flag(FLAG_CF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x1200);
}

#[test]
fn opcode_82_aliases_80_add() {
    // ADD AL, 5 encoded with the undocumented 0x82 group-1 opcode.
    let (mut cpu, memory) = real_mode_cpu(&[0x82, 0xc0, 0x05], 0x10);
    cpu.write_reg16(Reg16::Ax, 0x0010);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax) & 0xff, 0x15);
}

#[test]
fn wait_is_a_nop_without_fpu() {
    let (mut cpu, memory) = real_mode_cpu(&[0x9b], 0x10);
    let flags_before = cpu.registers.eflags;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 1);
    assert_eq!(cpu.registers.eflags, flags_before);
}

// ---- Phase 2 slice A: x87 FPU foundation (see dev_docs/coverage-roadmap.md) ----

#[test]
fn fninit_then_fld1_pushes_one() {
    // FNINIT (DB E3) then FLD1 (D9 E8).
    let (mut cpu, memory) = real_mode_cpu(&[0xdb, 0xe3, 0xd9, 0xe8], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.top(), 7);
}

#[test]
fn fld_fadd_fstp_round_trips_m64() {
    // FLD m64 [0x100]; FADD m64 [0x108]; FSTP m64 [0x110]. 2.5 + 1.25 = 3.75.
    let code = [
        0xdd, 0x06, 0x00, 0x01, 0xdc, 0x06, 0x08, 0x01, 0xdd, 0x1e, 0x10, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.5f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&1.25f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    let stored = f64::from_le_bytes(bus.memory[0x110..0x118].try_into().unwrap());
    assert_eq!(stored, 3.75);
}

#[test]
fn fxch_swaps_st0_and_st1() {
    // FLD1 (D9 E8); FLDZ (D9 EE); FXCH ST(1) (D9 C9). ST0 ends as 1.0.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xee, 0xd9, 0xc9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.get(1), 0.0);
}

#[test]
fn fnstsw_ax_reports_top_in_status() {
    // FLD1 (D9 E8) then FNSTSW AX (DF E0): TOP=7 lands in AX bits 11-13.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xdf, 0xe0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!((cpu.read_reg16(Reg16::Ax) >> 11) & 0x7, 7);
}

#[test]
fn fild_fmulp_fistp_integer_path() {
    // FILD m32 [0x100]=5; FILD m32 [0x104]=3; FMULP ST1,ST0 (DE C9); FISTP m32 [0x108].
    let code = [
        0xdb, 0x06, 0x00, 0x01, 0xdb, 0x06, 0x04, 0x01, 0xde, 0xc9, 0xdb, 0x1e, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&5i32.to_le_bytes());
    memory[0x104..0x108].copy_from_slice(&3i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    let stored = i32::from_le_bytes(bus.memory[0x108..0x10c].try_into().unwrap());
    assert_eq!(stored, 15);
}

#[test]
fn fsub_reverse_forms_differ() {
    // D8 /5 FSUBR ST0,ST(i): ST0 = ST(i) - ST0. Start ST0=2, ST1=10 -> 8.
    // FLD m64 [0x100]=10; FLD m64 [0x108]=2; FSUBR ST0,ST1 (D8 E9).
    let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd8, 0xe9];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&10.0f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&2.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 8.0);
}

// ---- Phase 2 slice B: x87 transcendentals ----

#[test]
fn f2xm1_of_one_is_one() {
    // FLD1 (D9 E8); F2XM1 (D9 F0): 2^1 - 1 = 1.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xf0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fyl2x_computes_y_times_log2_x() {
    // FLD1 (ST1=1); FLD m64 [0x100]=2 (ST0=2); FYL2X (D9 F1): 1 * log2(2) = 1.
    let code = [0xd9, 0xe8, 0xdd, 0x06, 0x00, 0x01, 0xd9, 0xf1];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 1.0);
}

#[test]
fn fscale_scales_by_power_of_two() {
    // FLD m64 [0x100]=2 (ST1); FLD m64 [0x108]=3 (ST0); FSCALE (D9 FD): 3 * 2^2 = 12.
    let code = [0xdd, 0x06, 0x00, 0x01, 0xdd, 0x06, 0x08, 0x01, 0xd9, 0xfd];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&2.0f64.to_le_bytes());
    memory[0x108..0x110].copy_from_slice(&3.0f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 12.0);
}

#[test]
fn fptan_replaces_st0_and_pushes_one() {
    // FLDZ (D9 EE); FPTAN (D9 F2): tan(0)=0 in ST1, 1.0 pushed into ST0.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xee, 0xd9, 0xf2], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.0);
    assert_eq!(cpu.fpu.get(1), 0.0);
}

// ---- Phase 2 slice C: integer-operand arithmetic + 80-bit extended ----

#[test]
fn fidiv_divides_by_an_integer_operand() {
    // FILD m32 [0x100]=20; FIDIV m32 [0x104]=4 (DA /6). 20 / 4 = 5.
    let code = [0xdb, 0x06, 0x00, 0x01, 0xda, 0x36, 0x04, 0x01];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&20i32.to_le_bytes());
    memory[0x104..0x108].copy_from_slice(&4i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 5.0);
}

#[test]
fn extended80_round_trips_through_memory() {
    // FLD m64 [0x100]=3.5; FSTP m80 [0x108] (DB /7); FLD m80 [0x108] (DB /5).
    let code = [
        0xdd, 0x06, 0x00, 0x01, 0xdb, 0x3e, 0x08, 0x01, 0xdb, 0x2e, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&3.5f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 3.5);
}

// ---- Phase 2 slice D: BCD, environment, state save/restore, FUCOMPP ----

#[test]
fn fbld_fbstp_round_trips_packed_bcd() {
    // FILD m32 [0x100]=12345; FBSTP m80 [0x108] (DF /6); FBLD m80 [0x108] (DF /4).
    let code = [
        0xdb, 0x06, 0x00, 0x01, 0xdf, 0x36, 0x08, 0x01, 0xdf, 0x26, 0x08, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x104].copy_from_slice(&12345i32.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 12345.0);
}

#[test]
fn fnsave_frstor_round_trips_registers() {
    // FLD1; FLD m64 [0x180]=2.5; FNSAVE [0x100] (DD /6); FRSTOR [0x100] (DD /4).
    let code = [
        0xd9, 0xe8, 0xdd, 0x06, 0x80, 0x01, 0xdd, 0x36, 0x00, 0x01, 0xdd, 0x26, 0x00, 0x01,
    ];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x180..0x188].copy_from_slice(&2.5f64.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.get(0), 2.5);
    assert_eq!(cpu.fpu.get(1), 1.0);
}

#[test]
fn fnstenv_fldenv_round_trips_top() {
    // FLD1 (TOP=7); FNSTENV [0x100] (D9 /6); FNINIT (TOP=0); FLDENV [0x100] (D9 /4).
    let code = [
        0xd9, 0xe8, 0xd9, 0x36, 0x00, 0x01, 0xdb, 0xe3, 0xd9, 0x26, 0x00, 0x01,
    ];
    let (mut cpu, memory) = real_mode_cpu(&code, 0x200);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!(cpu.fpu.top(), 7);
}

#[test]
fn fucompp_sets_equal_condition() {
    // FLD1; FLD1; FUCOMPP (DA E9): equal -> C3 set, both popped.
    let (mut cpu, memory) = real_mode_cpu(&[0xd9, 0xe8, 0xd9, 0xe8, 0xda, 0xe9], 0x20);
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap();
    }
    assert_eq!((cpu.fpu.status >> 14) & 1, 1, "C3 set on equal");
    assert_eq!(cpu.fpu.top(), 0, "both operands popped");
}

// ---- Phase 3: MMX execute path (lane math is unit-tested in mmx.rs) ----

#[test]
fn movd_then_movq_copies_registers() {
    // MOVD mm0, eax (0F 6E C0); MOVQ mm1, mm0 (0F 6F C8).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x6f, 0xc8], 0x20);
    cpu.registers.set_eax(0x0a0b_0c0d);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.mm(0), 0x0a0b_0c0d);
    assert_eq!(cpu.fpu.mm(1), 0x0a0b_0c0d);
}

#[test]
fn paddb_adds_packed_bytes_from_memory() {
    // MOVQ mm0, [0x100]; PADDB mm0, [0x108].
    let code = [0x0f, 0x6f, 0x06, 0x00, 0x01, 0x0f, 0xfc, 0x06, 0x08, 0x01];
    let (mut cpu, mut memory) = real_mode_cpu(&code, 0x200);
    memory[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    memory[0x108..0x110].copy_from_slice(&[10, 10, 10, 10, 10, 10, 10, 10]);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.fpu.mm(0).to_le_bytes(),
        [11, 12, 13, 14, 15, 16, 17, 18]
    );
}

#[test]
fn emms_marks_the_x87_stack_empty() {
    // MOVD mm0, eax marks the tags valid; EMMS (0F 77) empties them.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x77], 0x20);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.tag, 0x0000, "MMX write marks tags valid");
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.fpu.tag, 0xffff, "EMMS empties the tag word");
}

#[test]
fn psllw_immediate_shifts_each_word() {
    // MOVD mm0, eax (0x0001_0002); PSLLW mm0, 4 (0F 71 /6 imm8).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x6e, 0xc0, 0x0f, 0x71, 0xf0, 0x04], 0x20);
    cpu.registers.set_eax(0x0001_0002);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    cpu.cycle(&mut bus).unwrap();
    // low dword loaded; word lanes 0x0002 and 0x0001 each shift left by 4.
    assert_eq!(cpu.fpu.mm(0) & 0xffff_ffff, 0x0010_0020);
}

// ---- Phase 4 slice A: protected-mode system instructions ----

/// Protected-mode CPU with a GDT (base 0x100, limit 0x1f) holding one descriptor
/// at selector 0x08. CS selector 0 => CPL 0.
fn protected_cpu(code: &[u8], descriptor_low: u32, descriptor_high: u32) -> (Cpu386, Vec<u8>) {
    let mut memory = vec![0u8; 0x200];
    memory[..code.len()].copy_from_slice(code);
    memory[0x108..0x10c].copy_from_slice(&descriptor_low.to_le_bytes());
    memory[0x10c..0x110].copy_from_slice(&descriptor_high.to_le_bytes());
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0x1f,
    };
    cpu.control.cr0 |= CR0_PE;
    (cpu, memory)
}

#[test]
fn smsw_stores_machine_status_word() {
    // SMSW eax (0F 01 E0).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xe0], 0x20);
    cpu.control.cr0 = CR0_TS | CR0_MP; // 0x0A
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eax() & 0xffff, 0x000a);
}

#[test]
fn lmsw_sets_protection_enable() {
    // LMSW ax (0F 01 F0) with AX bit 0 set turns on CR0.PE.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0xf0], 0x20);
    cpu.write_reg16(Reg16::Ax, 0x0001);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_ne!(cpu.control.cr0 & CR0_PE, 0);
}

#[test]
fn clts_clears_task_switched() {
    // CLTS (0F 06).
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x06], 0x20);
    cpu.control.cr0 |= CR0_TS;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.control.cr0 & CR0_TS, 0);
}

#[test]
fn sgdt_stores_the_gdtr() {
    // SGDT [0x100] (0F 01 06 00 01): limit word then base dword.
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x01, 0x06, 0x00, 0x01], 0x200);
    cpu.gdtr = DescriptorTable {
        base: 0x1234_5678,
        limit: 0x0abc,
    };
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        u16::from_le_bytes([bus.memory[0x100], bus.memory[0x101]]),
        0x0abc
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x102..0x106].try_into().unwrap()),
        0x1234_5678
    );
}

#[test]
fn sldt_stores_the_ldtr_selector() {
    // SLDT ax (0F 00 C0), protected mode only.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xc0], 0, 0);
    cpu.ldtr.selector = 0x0028;
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x0028);
}

#[test]
fn sldt_is_invalid_in_real_mode() {
    let (mut cpu, memory) = real_mode_cpu(&[0x0f, 0x00, 0xc0], 0x20);
    let mut bus = TestBus::with_memory(memory);
    // SLDT (0F 00 /0) is converted to the decode/execute split (task A12); the whole 0F 00
    // group is #UD outside protected mode, raised in `execute_system_seg_decoded`.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 6, .. }));
}

#[test]
fn a12_unimplemented_neighbours_stay_undefined() {
    // The A12-adjacent opcodes that the fused path never implemented must NOT be routed into the
    // new SystemSeg split — they remain on Fallback / TwoByteFallback and #UD as before. Guard
    // the routing so a future edit can't silently capture them. 0x63 (ARPL) #UDs (vector 6, no
    // error code) through the split, never a panic. (0F B2/B4/B5 LSS/LFS/LGS and 0F 21/23 MOV
    // reg,DR / MOV DR,reg moved OFF this list once implemented -- see the `lss`/`lfs`/`lgs` tests
    // below and the ledger-row-25 `mov_dr*` tests above.)
    let code = &[0x63, 0xc0][..]; // ARPL AX, AX
    let (mut cpu, memory) = real_mode_cpu(code, 0x40);
    let mut bus = TestBus::with_memory(memory);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "expected an unsupported-opcode error for {code:02x?}, got {err:?}"
    );
}

// ---- PUSH/POP FS/GS (0F A0/A1/A8/A9) ----

/// Real-mode CPU with SP parked at 0x1f0 (mirrors `stack_seed`) and 0x200 bytes of memory,
/// so PUSH/POP FS/GS have room on the stack. Used for the real-mode + 16/32-bit-operand-size
/// arms of the new opcodes; the protected-mode descriptor-load arms use `protected_cpu` below.
fn fs_gs_stack_cpu(code: &[u8]) -> (Cpu386, Vec<u8>) {
    let mut memory = vec![0u8; 0x200];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x01f0);
    (cpu, memory)
}

#[test]
fn push_fs_pushes_the_selector_in_real_mode() {
    // PUSH FS (0F A0). Mirrors PUSH DS (0x1e): pushes the 16-bit selector, SP -= 2.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa0]);
    cpu.registers
        .set_segment(SegmentIndex::Fs, SegmentRegister::real(0x1234));
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01ee, "SP must decrement by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x1ee..0x1f0].try_into().unwrap()),
        0x1234,
        "FS selector must land on the stack"
    );
}

#[test]
fn push_gs_pushes_the_selector_in_real_mode() {
    // PUSH GS (0F A8). Mirrors PUSH DS (0x1e).
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa8]);
    cpu.registers
        .set_segment(SegmentIndex::Gs, SegmentRegister::real(0x5678));
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01ee, "SP must decrement by 2");
    assert_eq!(
        u16::from_le_bytes(bus.memory[0x1ee..0x1f0].try_into().unwrap()),
        0x5678,
        "GS selector must land on the stack"
    );
}

#[test]
fn push_fs_gs_zero_extend_under_the_32_bit_operand_size_prefix() {
    // 66 0F A0 / 66 0F A8: 386 PRM -- PUSH sreg with a 32-bit operand size decrements
    // ESP by 4 and writes the 16-bit selector zero-extended to a dword (the SDM PUSH
    // operation note). Same rule as the one-byte PUSH ES/CS/SS/DS arms.
    for (code, segment, value) in [
        ([0x66u8, 0x0f, 0xa0].as_slice(), SegmentIndex::Fs, 0x1234u16),
        ([0x66, 0x0f, 0xa8].as_slice(), SegmentIndex::Gs, 0x5678u16),
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(code);
        cpu.registers
            .set_segment(segment, SegmentRegister::real(value));
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ec,
            "SP must move by 4 with a 32-bit operand-size prefix"
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
            u32::from(value),
            "the pushed dword must be the 16-bit selector zero-extended"
        );
    }
}

#[test]
fn pop_fs_gs_discard_the_upper_word_under_the_32_bit_operand_size_prefix() {
    // 66 0F A1 / 66 0F A9: 386 PRM -- POP sreg with a 32-bit operand size pops a full
    // dword, loads the low 16 bits into the segment register, and discards the upper 16.
    // Same rule as the one-byte POP ES/SS/DS arms.
    for (code, segment) in [
        ([0x66u8, 0x0f, 0xa1].as_slice(), SegmentIndex::Fs),
        ([0x66, 0x0f, 0xa9].as_slice(), SegmentIndex::Gs),
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f4, "SP must advance by 4");
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0xbeef,
            "{segment:?} must load only the low 16 bits, discarding 0xdead"
        );
    }
}

#[test]
fn push_pop_one_byte_sreg_zero_extend_under_the_32_bit_operand_size_prefix() {
    // 66 06 / 66 0E / 66 16 / 66 1E (PUSH ES/CS/SS/DS) and 66 07 / 66 1F (POP ES/DS):
    // the one-byte segment-register push/pop opcodes follow the identical 386 PRM
    // operand-size rule as PUSH/POP FS/GS above. POP SS (66 17) is covered separately
    // below because it arms the MOV-SS interrupt shadow.
    for (push_code, pop_code, segment, value) in [
        (
            [0x66u8, 0x06].as_slice(),
            [0x66u8, 0x07].as_slice(),
            SegmentIndex::Es,
            0x1111u16,
        ),
        (
            [0x66, 0x1e].as_slice(),
            [0x66, 0x1f].as_slice(),
            SegmentIndex::Ds,
            0x2222,
        ),
    ] {
        // PUSH: selector zero-extended to a dword, ESP -= 4.
        let (mut cpu, memory) = fs_gs_stack_cpu(push_code);
        cpu.registers
            .set_segment(segment, SegmentRegister::real(value));
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ec,
            "{push_code:02x?}: SP must move by 4 with a 32-bit operand-size prefix"
        );
        assert_eq!(
            u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
            u32::from(value),
            "{push_code:02x?}: the pushed dword must be the selector zero-extended"
        );

        // POP: full dword popped, only the low 16 bits loaded.
        let (mut cpu, memory) = fs_gs_stack_cpu(pop_code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01f4,
            "{pop_code:02x?}: SP must advance by 4"
        );
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0xbeef,
            "{pop_code:02x?}: {segment:?} must load only the low 16 bits"
        );
    }
}

#[test]
fn push_pop_ss_zero_extends_under_the_32_bit_operand_size_prefix() {
    // 66 16 (PUSH SS) / 66 17 (POP SS): same 386 PRM operand-size rule, but POP SS also
    // arms the one-instruction interrupt shadow (`load_segment_arming_ss_shadow`), so it
    // gets its own test rather than folding into the ES/DS table above. Unlike PUSH
    // FS/ES/DS, PUSH SS cannot push an arbitrary probe value into SS without also
    // relocating the stack it is about to push onto, so this asserts against
    // `fs_gs_stack_cpu`'s real-mode SS selector (0) instead.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x66, 0x16]);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x01ec,
        "PUSH SS must move SP by 4"
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x01ec..0x01f0].try_into().unwrap()),
        0x0000,
        "PUSH SS must zero-extend the selector to a dword"
    );

    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x66, 0x17]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f4].copy_from_slice(&0xdead_beefu32.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x01f4,
        "POP SS must advance SP by 4"
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0xbeef,
        "POP SS must load only the low 16 bits"
    );
}

#[test]
fn push_pop_one_byte_sreg_unchanged_at_16_bit_operand_size() {
    // Without a 66h prefix, PUSH/POP ES/CS/SS/DS (and FS/GS) stay the classic 2-byte
    // real-mode DOS behavior -- this is the frozen-class-sensitivity check: no bench or
    // real-mode DOS code observes a behavior change from the operand_size fix.
    for code in [
        [0x06u8, 0x90], // PUSH ES; NOP pad
        [0x0e, 0x90],   // PUSH CS; NOP pad
        [0x16, 0x90],   // PUSH SS; NOP pad
        [0x1e, 0x90],   // PUSH DS; NOP pad
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(&code);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01ee,
            "{code:02x?}: 16-bit-operand-size PUSH sreg must still only move SP by 2"
        );
    }
    for code in [
        [0x07u8, 0x90], // POP ES; NOP pad
        [0x1f, 0x90],   // POP DS; NOP pad
    ] {
        let (mut cpu, memory) = fs_gs_stack_cpu(&code);
        let mut bus = TestBus::with_memory(memory);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.read_reg16(Reg16::Sp),
            0x01f2,
            "{code:02x?}: 16-bit-operand-size POP sreg must still only move SP by 2"
        );
    }
}

#[test]
fn pop_fs_loads_the_selector_in_real_mode() {
    // POP FS (0F A1). Mirrors POP DS (0x1f): pops a 16-bit selector and loads it, SP += 2.
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa1]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must increment by 2");
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0xbeef);
}

#[test]
fn pop_gs_loads_the_selector_in_real_mode() {
    // POP GS (0F A9). Mirrors POP DS (0x1f).
    let (mut cpu, memory) = fs_gs_stack_cpu(&[0x0f, 0xa9]);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must increment by 2");
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0xbeef);
}

#[test]
fn pop_fs_gs_load_a_valid_descriptor_in_protected_mode() {
    // Data segment access 0x92 (present, data, writable), byte-granular limit 0xffff,
    // base 0 -- the same descriptor shape `verr_sets_zf_for_a_readable_segment` and
    // `lar_and_lsl_read_descriptor_fields` use. Selector 0x0008 (GDT index 1, RPL 0).
    for (code, segment) in [
        ([0x0fu8, 0xa1].as_slice(), SegmentIndex::Fs), // POP FS
        ([0x0f, 0xa9].as_slice(), SegmentIndex::Gs),   // POP GS
    ] {
        let (mut cpu, memory) = protected_cpu(code, 0x0000_ffff, 0x0000_9200);
        let mut bus = TestBus::with_memory(memory);
        cpu.write_reg16(Reg16::Sp, 0x01f0);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0x0008u16.to_le_bytes());
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0x0008,
            "{segment:?} selector must load"
        );
        assert_eq!(
            cpu.registers.segment(segment).base,
            0,
            "{segment:?} base must come from the descriptor"
        );
        assert_eq!(
            cpu.registers.segment(segment).limit,
            0xffff,
            "{segment:?} limit must come from the descriptor"
        );
        assert_eq!(cpu.read_reg16(Reg16::Sp), 0x01f2, "SP must advance by 2");
    }
}

#[test]
fn pop_fs_gs_fault_on_a_bad_selector_in_protected_mode() {
    // Selector 0x0028 (index 5, byte offset 40) is past the GDT limit of 0x1f (31), which
    // only covers offsets 0 (null) and 8 (the one installed descriptor), so the descriptor
    // load must #GP -- the same fault a bad POP DS selector raises.
    for (code, name) in [
        ([0x0fu8, 0xa1].as_slice(), "POP FS"),
        ([0x0f, 0xa9].as_slice(), "POP GS"),
    ] {
        let (mut cpu, memory) = protected_cpu(code, 0x0000_ffff, 0x0000_9200);
        let mut bus = TestBus::with_memory(memory);
        cpu.write_reg16(Reg16::Sp, 0x01f0);
        bus.memory[0x1f0..0x1f2].copy_from_slice(&0x0028u16.to_le_bytes());
        let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
        assert!(
            matches!(
                err,
                InternalFault::Exception {
                    vector: 13,
                    error_code: Some(40)
                }
            ),
            "{name} with an out-of-limit selector must #GP(0x28), got {err:?}"
        );
    }
}

/// Like `run_at_level`, but seeds SP at 0x1f0 (mirroring `fs_gs_stack_cpu`/`stack_seed`) so
/// the PUSH FS/GS arms have room on the stack instead of wrapping SP into unmapped memory.
/// POP FS/GS only ever read what PUSH just wrote (or zero), so no separate POP variant needed.
fn run_at_level_with_stack(code: &[u8], level: CpuLevel) -> Result<CycleOutcome, InternalFault> {
    let mut memory = vec![0; 1024];
    memory[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.set_level(level);
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x01f0);
    let mut bus = TestBus::with_memory(memory);
    exec_one_split(&mut cpu, &mut bus)
}

#[test]
fn push_pop_fs_gs_raise_ud_at_i286() {
    // FS/GS are 386+ only. At the I286 level the check_two_byte_isa_gate must #UD all four
    // opcodes (via is_386plus_two_byte), the same gate MOVZX/BSF/etc go through, and they
    // must run cleanly from I386 up.
    for code in [
        [0x0fu8, 0xa0], // PUSH FS
        [0x0f, 0xa1],   // POP FS
        [0x0f, 0xa8],   // PUSH GS
        [0x0f, 0xa9],   // POP GS
    ] {
        assert!(
            matches!(
                run_at_level_with_stack(&code, CpuLevel::I286).unwrap_err(),
                InternalFault::Exception { vector: 6, .. }
            ),
            "{code:02x?} must #UD at I286"
        );
        assert!(
            run_at_level_with_stack(&code, CpuLevel::I386).is_ok(),
            "{code:02x?} must run at I386"
        );
        assert!(run_at_level_with_stack(&code, CpuLevel::I486).is_ok());
        assert!(run_at_level_with_stack(&code, CpuLevel::I586).is_ok());
    }
}

#[test]
fn lldt_loads_the_descriptor() {
    // LDT descriptor at selector 0x08: base 0x0004_0000, limit 0x0fff, access 0x82.
    let low = 0x0000_0fff; // limit low, base low 16 = 0
    let high = 0x0000_8204; // base[23:16]=0x04, access=0x82
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xd0], low, high);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.ldtr.selector, 0x0008);
    assert_eq!(cpu.ldtr.base, 0x0004_0000);
    assert_eq!(cpu.ldtr.limit, 0x0fff);
}

#[test]
fn null_selector_loads_into_data_segments_without_fault() {
    // MOV DS/ES/FS/GS, AX with AX = 0 (a null selector: index 0, TI 0). The 386 lets a
    // null selector load into a data segment with no fault; only a later memory access
    // through it #GPs. Descriptor bytes are irrelevant here (never read for a null load).
    for (opcode_reg, segment) in [
        (0xc0u8, SegmentIndex::Es), // MOV ES, AX (8E C0)
        (0xd8, SegmentIndex::Ds),   // MOV DS, AX (8E D8)
        (0xe0, SegmentIndex::Fs),   // MOV FS, AX (8E E0)
        (0xe8, SegmentIndex::Gs),   // MOV GS, AX (8E E8)
    ] {
        let (mut cpu, memory) = protected_cpu(&[0x8e, opcode_reg], 0x0000_ffff, 0x0000_9200);
        cpu.write_reg16(Reg16::Ax, 0x0000);
        let mut bus = TestBus::with_memory(memory);
        cpu.cycle(&mut bus).unwrap();
        assert_eq!(
            cpu.registers.segment(segment).selector,
            0x0000,
            "{segment:?} must load the null selector"
        );
        assert_eq!(
            cpu.registers.segment(segment).access & 0x80,
            0,
            "{segment:?} must install a not-present/unusable segment"
        );
    }
}

#[test]
fn access_through_a_null_data_segment_faults() {
    // MOV DS, AX (8E D8) with AX = 0 loads DS as null (no fault); a following memory
    // access through DS (MOV AL, [SI], opcode 8A 04) must then #GP -- the null segment's
    // base=0/limit=0 default fails the segment-limit check for any nonzero offset.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8, 0x8a, 0x04], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0000);
    cpu.write_reg16(Reg16::Si, 0x0010);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, AX: loads null, no fault.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "access through a null DS must fault, got {fault:?}"
    );
}

#[test]
fn null_selector_into_ss_still_faults() {
    // MOV SS, AX (8E D0) with AX = 0. Unlike the data segments, a null selector loaded
    // into SS must still #GP -- the stack segment can never be null.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd0], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0000);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "a null selector into SS must #GP, got {fault:?}"
    );
}

#[test]
fn ldt_selector_resolves_against_the_ldt_not_the_gdt() {
    // Install an LDT (via LLDT) whose own descriptor lives at GDT selector 0x08, then load
    // DS from an LDT selector (TI=1, index 1: selector 0x000c) whose descriptor lives at
    // LDT offset 8. The GDT selector 0x08 descriptor is deliberately a system (LDT)
    // descriptor, not a data segment: if a test regression accidentally indexed the GDT
    // instead of the LDT for the DS load, it would read this LDT-type descriptor and the
    // base/limit assertions below would fail.
    let mut memory = vec![0u8; 0x400];
    // GDT at 0x100 (base/limit set by protected_cpu below): selector 0x08 is the LDT
    // system descriptor (base 0x0000_0200, limit 0x0f, access 0x82 = present, LDT type).
    let ldt_desc_low = 0x0200_000f; // limit low = 0x0f, base[15:0] = 0x0200
    let ldt_desc_high = 0x0000_8200; // base[31:24]=0, base[23:16]=0, access = 0x82 (present LDT)
    let (mut cpu, mut code) =
        protected_cpu(&[0x0f, 0x00, 0xd0, 0x8e, 0xd9], ldt_desc_low, ldt_desc_high);
    code.resize(0x400, 0);
    // LDT lives at 0x200 (matches the descriptor base above). LDT selector 0x000c is
    // index 1 (byte offset 8) inside the LDT: a data segment, base 0x0005_0000, limit
    // 0x00ff, access 0x92 (present, data, writable).
    let ldt_base = 0x200usize;
    code[ldt_base + 8..ldt_base + 12].copy_from_slice(&0x0000_00ffu32.to_le_bytes());
    code[ldt_base + 12..ldt_base + 16].copy_from_slice(&0x0000_9205u32.to_le_bytes());
    memory[..code.len()].copy_from_slice(&code);
    cpu.write_reg16(Reg16::Ax, 0x0008); // LLDT AX: load LDTR from GDT selector 0x08.
    cpu.write_reg16(Reg16::Cx, 0x000c); // MOV DS, CX: load DS from LDT selector 0x000c.
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // LLDT AX
    assert_eq!(cpu.ldtr.base, 0x0000_0200);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, CX
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x000c);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).base,
        0x0005_0000,
        "DS must resolve against the LDT descriptor, not the GDT"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0x00ff);
}

#[test]
fn gdt_selector_still_loads_after_the_ldt_fix() {
    // A plain GDT selector (TI=0) must still resolve against the GDT: regression guard for
    // the TI-bit fix in `load_protected_segment`.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x0008);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0xffff);
}

#[test]
fn out_of_limit_selector_still_faults() {
    // Selector 0x0028 (index 5) is past the GDT limit of 0x1f installed by `protected_cpu`
    // (which only covers offsets 0 and 8): a genuinely invalid, non-null selector must
    // still #GP, unaffected by the null-selector and LDT fixes.
    let (mut cpu, memory) = protected_cpu(&[0x8e, 0xd8], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0028);
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(40)
            }
        ),
        "an out-of-limit selector must #GP(0x28), got {fault:?}"
    );
}

#[test]
fn retf_popping_a_null_selector_into_cs_faults() {
    // RETF (0xcb) in protected mode with the stacked far pointer's selector word set to
    // 0x0000 (null, index 0, TI 0). Unlike a data segment, CS must never be null: this
    // exercises load_segment(..., SegmentIndex::Cs, ...) through the real RETF path
    // (return_far -> load_segment -> load_protected_segment), not a synthetic direct call,
    // so it also confirms IRET/interrupt-gate delivery's CS reload would fault the same way.
    let (mut cpu, mut memory) = protected_cpu(&[0xcb], 0x0000_ffff, 0x0000_9200);
    memory.resize(0x200, 0);
    cpu.registers.set_esp(0x0100);
    // Stacked far pointer at ss:0x0100: offset 0x1234, then selector 0x0000 (null).
    memory[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&0x0000u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            fault,
            InternalFault::Exception {
                vector: 13,
                error_code: Some(0)
            }
        ),
        "RETF popping a null selector into CS must #GP, got {fault:?}"
    );
}

#[test]
fn ti_bit_set_index_zero_selector_resolves_against_the_ldt_not_treated_as_null() {
    // Selector 0x0004: index 0, TI=1. This is NOT a null selector (only index 0 AND TI 0
    // is null) -- it must resolve against LDT offset 0, not short-circuit into the
    // null/unusable path. Install an LDT (via LLDT, GDT selector 0x08) whose first entry
    // (offset 0) is a normal data descriptor, then load DS from selector 0x0004 and check
    // the resulting base/limit came from that LDT descriptor.
    let mut memory = vec![0u8; 0x400];
    // GDT selector 0x08: LDT system descriptor, base 0x0000_0300, limit 0x0f, access 0x82.
    let ldt_desc_low = 0x0300_000f;
    let ldt_desc_high = 0x0000_8200;
    let (mut cpu, mut code) =
        protected_cpu(&[0x0f, 0x00, 0xd0, 0x8e, 0xd9], ldt_desc_low, ldt_desc_high);
    code.resize(0x400, 0);
    // LDT at 0x300 (matches the descriptor base above). LDT offset 0 (selector 0x0004,
    // index 0, TI 1): data segment, base 0x0006_0000, limit 0x00aa, access 0x92.
    let ldt_base = 0x300usize;
    code[ldt_base..ldt_base + 4].copy_from_slice(&0x0000_00aau32.to_le_bytes());
    code[ldt_base + 4..ldt_base + 8].copy_from_slice(&0x0000_9206u32.to_le_bytes());
    memory[..code.len()].copy_from_slice(&code);
    cpu.write_reg16(Reg16::Ax, 0x0008); // LLDT AX.
    cpu.write_reg16(Reg16::Cx, 0x0004); // MOV DS, CX: selector 0x0004 (index 0, TI 1).
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap(); // LLDT AX
    assert_eq!(cpu.ldtr.base, 0x0000_0300);
    cpu.cycle(&mut bus).unwrap(); // MOV DS, CX
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x0004);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).base,
        0x0006_0000,
        "index-0/TI-1 selector 0x0004 must resolve against LDT[0], not be treated as null"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).limit, 0x00aa);
    assert_ne!(
        cpu.registers.segment(SegmentIndex::Ds).access & 0x80,
        0,
        "a resolved LDT descriptor load must install a present segment, not the null/unusable default"
    );
}

#[test]
fn verr_sets_zf_for_a_readable_segment() {
    // Readable data segment: access 0x92 (P, S, data, writable -> readable).
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x00, 0xe0], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Ax, 0x0008);
    cpu.set_flag(FLAG_ZF, false);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.flag(FLAG_ZF),
        "VERR should set ZF for a readable segment"
    );
}

#[test]
fn lar_and_lsl_read_descriptor_fields() {
    // Data segment access 0x92, byte-granular limit 0xffff.
    // LAR ax, cx (0F 02 C1); CX holds the selector.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x02, 0xc1], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0x9200);

    // LSL ax, cx (0F 03 C1) -> the byte-granular limit.
    let (mut cpu, memory) = protected_cpu(&[0x0f, 0x03, 0xc1], 0x0000_ffff, 0x0000_9200);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.flag(FLAG_ZF));
    assert_eq!(cpu.read_reg16(Reg16::Ax), 0xffff);
}

// ---- Phase 4 slice B: exception error codes and FPU #MF ----

#[test]
fn error_code_vectors_are_classified() {
    for v in [8u8, 10, 11, 12, 13, 14, 17] {
        assert!(
            vector_pushes_error_code(v),
            "vector {v} should carry a code"
        );
    }
    for v in [0u8, 1, 3, 4, 5, 6, 7, 9, 16, 18, 19] {
        assert!(!vector_pushes_error_code(v), "vector {v} carries no code");
    }
}

/// FLDZ; FLD1; FDIV ST0,ST1 (divide 1 by 0); FWAIT.
const DIV_BY_ZERO_THEN_WAIT: [u8; 7] = [0xd9, 0xee, 0xd9, 0xe8, 0xd8, 0xf1, 0x9b];

#[test]
fn unmasked_divide_by_zero_traps_mf_on_fwait() {
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    cpu.fpu.control = 0x037b; // default mask with ZM (bit 2) cleared
    cpu.control.cr0 |= CR0_NE;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..3 {
        cpu.cycle(&mut bus).unwrap(); // FLDZ, FLD1, FDIV
    }
    assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag set");
    // FWAIT (0x9b) is now on the decode/execute split (its fused arm is gone), so drive it
    // through `exec_one_split` rather than the legacy fused entry, which would #UD it.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 16, .. }));
}

#[test]
fn masked_divide_by_zero_does_not_trap() {
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    // Default control 0x037F masks every exception.
    cpu.control.cr0 |= CR0_NE;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap(); // FWAIT retires normally
    }
    assert_ne!(cpu.fpu.status & 0x04, 0, "ZE flag still latched");
}

#[test]
fn mf_is_suppressed_when_ne_is_clear() {
    // Unmasked exception but CR0.NE clear: the PC's FERR/IRQ13 path applies, so no
    // internal #MF. FWAIT retires.
    let (mut cpu, memory) = real_mode_cpu(&DIV_BY_ZERO_THEN_WAIT, 0x40);
    cpu.fpu.control = 0x037b;
    let mut bus = TestBus::with_memory(memory);
    for _ in 0..4 {
        cpu.cycle(&mut bus).unwrap();
    }
}

// ---- Phase 4 slice C: call gates and privilege-level stack switching ----

/// Protected-mode CPU with a GDT at 0x100 holding the given (selector, low, high)
/// descriptors. CS/SS default to ring 0 (real-mode shells, base 0); SP at 0x80.
fn protected_cpu_with_gdt(code: &[u8], descriptors: &[(u16, u32, u32)]) -> (Cpu386, Vec<u8>) {
    let mut memory = vec![0u8; 0x400];
    memory[..code.len()].copy_from_slice(code);
    for &(sel, low, high) in descriptors {
        let off = 0x100 + (sel & !0x7) as usize;
        memory[off..off + 4].copy_from_slice(&low.to_le_bytes());
        memory[off + 4..off + 8].copy_from_slice(&high.to_le_bytes());
    }
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x80);
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0xff,
    };
    cpu.control.cr0 |= CR0_PE;
    (cpu, memory)
}

// Flat ring-0 code at 0x08, and a 386 call gate at 0x10 -> 0x08:0x40.
const RING0_CODE: (u16, u32, u32) = (0x08, 0x0000_ffff, 0x00cf_9b00);
const CALL_GATE_DPL0: (u16, u32, u32) = (0x10, 0x0008_0040, 0x0000_8c00);

#[test]
fn call_gate_same_privilege_transfers() {
    // CALL FAR 0x10:0 -> through the gate to 0x08:0x40, return pushed.
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x10, 0x00],
        &[RING0_CODE, CALL_GATE_DPL0],
    );
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x08);
    assert_eq!(cpu.registers.eip, 0x40);
    // The gate is a 386 (32-bit) gate, so the return CS:EIP is two dwords.
    assert_eq!(
        cpu.registers.esp(),
        0x80 - 8,
        "return offset+selector pushed"
    );
}

#[test]
fn jmp_gate_transfers_without_pushing_return() {
    // JMP FAR 0x10:0 -> same target, no return frame.
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x10, 0x00],
        &[RING0_CODE, CALL_GATE_DPL0],
    );
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x08);
    assert_eq!(cpu.registers.eip, 0x40);
    assert_eq!(cpu.registers.esp(), 0x80, "JMP pushes nothing");
}

#[test]
fn call_gate_inter_privilege_switches_stack() {
    // Ring-3 caller through a DPL-3 gate into ring-0 code, copying two dword params.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl3 = (0x30u16, 0x0008_0040, 0x0000_ec02); // DPL3 386 gate, 2 params
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl3],
    );
    // Run at CPL 3 with a ring-3 CS and SS (set the cached registers directly).
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false,
        },
    );
    cpu.registers.set_esp(0xc0);
    cpu.cpl = 3; // this test sets CS/SS directly, so seed the cached CPL to match
    // Two parameters on the outer stack.
    memory[0xc0..0xc4].copy_from_slice(&0x1111u32.to_le_bytes());
    memory[0xc4..0xc8].copy_from_slice(&0x2222u32.to_le_bytes());
    // TSS at 0x300 with the ring-0 stack: ESP0 at +4, SS0 at +8.
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "entered ring-0 code");
    assert_eq!(cpu.registers.eip, 0x40);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        0x10,
        "switched to SS0"
    );
    // Frame on the new stack: 6 dwords pushed below ESP0 = 0xF0.
    assert_eq!(cpu.registers.esp(), 0xf0 - 24);
    // Return EIP (5, past the CALL) at the top; param0 above the return frame.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xd8..0xdc].try_into().unwrap()),
        5
    );
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xe0..0xe4].try_into().unwrap()),
        0x1111
    );
}

#[test]
fn call_gate_inter_privilege_reads_params_from_a_16bit_outer_stack_with_esp_high_garbage() {
    // The DOS4GW/VCPI scenario: the outer (caller's) stack is SS.B=0 with garbage
    // in ESP's high word. Per PRM 17-42 the old stack's top is SS:SP -- the param
    // read must use the wrapped 16-bit SP, not outer_esp + k*psize on the full
    // (garbage-laden) ESP, which would read from a bogus linear address entirely
    // outside the intended stack page.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl1 = (0x30u16, 0x0008_0040, 0x0000_ec01); // DPL3 386 gate, 1 param
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl1],
    );
    memory.resize(0x1_0004, 0);
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false, // SS.B=0: the outer stack is 16-bit.
        },
    );
    // SP = 0xfffe, but ESP's high word carries garbage that a full-ESP add would
    // corrupt into a wrong linear address entirely -- the wrapped SP is the only
    // correct read point. The single param sits at SP=0xfffe, well clear of the
    // 5-byte CALL instruction at offset 0.
    cpu.registers.set_esp(0xbeef_fffe);
    cpu.cpl = 3;
    memory[0xfffe..0x1_0002].copy_from_slice(&0x1111u32.to_le_bytes());
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "entered ring-0 code");
    assert_eq!(cpu.registers.eip, 0x40);
    // Frame on the new stack: return CS:EIP + old SS:ESP + 1 param = 5 dwords.
    assert_eq!(cpu.registers.esp(), 0xf0 - 20);
    // The param, pushed just above the return frame, must be the value at the
    // wrapped SP 0xfffe, not whatever garbage a full-ESP read would have hit.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0xe4..0xe8].try_into().unwrap()),
        0x1111,
        "param read from the wrapped SP, not full-ESP garbage"
    );
}

// ---- CPL transition unit tests (the `cpl` field, one per PRM transition-point
// class named in the VCPI substrate fix). Each drives a real transfer through
// `cycle`/`deliver_exception`/`iret` and asserts `current_privilege_level()`
// lands where the PRM says, not merely that a CS selector's low bits look right.

#[test]
fn cpl_transition_call_gate_inter_privilege_call_lowers_cpl_to_target_dpl() {
    // Reuses the exact fixture from `call_gate_inter_privilege_switches_stack`:
    // a ring-3 caller through a DPL-3 gate into ring-0 code. The cached CPL must
    // read 0 once inside the gate's target, not just the CS selector's RPL.
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let gate_dpl3 = (0x30u16, 0x0008_0040, 0x0000_ec02);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0x9a, 0x00, 0x00, 0x30, 0x00],
        &[RING0_CODE, ring0_data, gate_dpl3],
    );
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x1b,
            base: 0,
            limit: 0xf_ffff,
            access: 0xfb,
            default_size_32: false,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x23,
            base: 0,
            limit: 0xf_ffff,
            access: 0xf3,
            default_size_32: false,
        },
    );
    cpu.registers.set_esp(0xc0);
    cpu.cpl = 3;
    cpu.tr.base = 0x300;
    memory[0x304..0x308].copy_from_slice(&0x00f0u32.to_le_bytes());
    memory[0x308..0x30a].copy_from_slice(&0x0010u16.to_le_bytes());
    let mut bus = TestBus::with_memory(memory);

    assert_eq!(cpu.current_privilege_level(), 3, "starts at CPL 3");
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.current_privilege_level(),
        0,
        "the call gate's target DPL (0) is the new CPL"
    );
}

#[test]
fn cpl_transition_far_jmp_direct_tracks_the_loaded_cs_rpl() {
    // A direct (non-gate) far JMP to a flat code segment: no privilege check is
    // enforced on this path today, but the cached CPL must still track whatever
    // CS RPL the jump landed on (same live-formula answer as before, just cached).
    let target = (0x20u16, 0x0000_ffff, 0x00cf_fb00); // DPL 3 code segment
    let (mut cpu, memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x23, 0x00], // JMP FAR 0x23:0 (RPL 3)
        &[RING0_CODE, target],
    );
    let mut bus = TestBus::with_memory(memory);
    assert_eq!(cpu.current_privilege_level(), 0, "starts at CPL 0");
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector & 3, 3);
    assert_eq!(
        cpu.current_privilege_level(),
        3,
        "direct far JMP's cached CPL follows the loaded CS RPL"
    );
}

#[test]
fn cpl_transition_iret_into_v86_forces_cpl_3() {
    // A ring-0 IRET whose popped EFLAGS carries VM=1 always lands at CPL 3,
    // regardless of the popped V86 CS's arbitrary selector bits.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2; // ring 0, no VM
    cpu.cpl = 0;
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    // 0x9000 is unused by `v86_world`'s memory map (PD/PT/GDT/IDT/TSS/ESP0/monitor
    // code all sit below it, guest code at 0xA000 above); avoids clobbering the
    // paging structures the way writing at 0x1000 (the PD itself) would.
    cpu.registers.set_esp(0x9000);
    // Build the V86-return IRET frame by hand: EIP, CS(0xFFFF, arbitrary low
    // bits), EFLAGS(VM=1), ESP, SS, ES, DS, FS, GS.
    // Lay out the frame in ascending-address (pop) order: IRET pops EIP, CS,
    // EFLAGS, then (VM=1 detected) ESP, SS, ES, DS, FS, GS.
    let mut write = |offset: u32, v: u32| {
        put32(&mut bus.memory, 0x9000 + offset, v);
    };
    write(0, 0x10); // EIP
    write(4, 0xffff); // CS (RPL bits arbitrary/irrelevant)
    write(8, FLAG_VM | 0x2); // EFLAGS
    write(12, 0x2000); // ESP
    write(16, 0x0900); // SS
    write(20, 0x1111); // ES
    write(24, 0x2222); // DS
    write(28, 0x3333); // FS
    write(32, 0x4444); // GS

    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "returned into V86");
    assert_eq!(
        cpu.current_privilege_level(),
        3,
        "IRET-into-V86 always forces CPL 3"
    );
}

#[test]
fn cpl_transition_pe_clear_resets_cpl_to_zero() {
    // MOV CR0, EAX clearing PE (require_cpl0-gated, so CPL was already 0): the
    // cache must stay 0 across the real-mode transition, matching real mode's
    // fixed CPL 0.
    let mut memory = vec![0u8; 16];
    memory[..3].copy_from_slice(&[0x0f, 0x22, 0xc0]); // MOV CR0, EAX
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0); // clears PE
    let mut bus = TestBus::with_memory(memory);

    assert_eq!(cpu.current_privilege_level(), 0);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.is_protected_mode(), "PE cleared");
    assert_eq!(
        cpu.current_privilege_level(),
        0,
        "real mode is always CPL 0"
    );
}

// ---- Phase 4 slice D: hardware task switch ----

#[test]
fn jmp_to_tss_performs_a_task_switch() {
    // New 386 TSS at 0x380 (selector 0x18), old busy TSS at 0x300 (selector 0x20).
    let new_tss = (0x18u16, 0x0380_0067, 0x0000_8900);
    let old_tss = (0x20u16, 0x0300_0067, 0x0000_8b00);
    let ring0_data = (0x10u16, 0x0000_ffff, 0x00cf_9300);
    let (mut cpu, mut memory) = protected_cpu_with_gdt(
        &[0xea, 0x00, 0x00, 0x18, 0x00],
        &[RING0_CODE, ring0_data, new_tss, old_tss],
    );
    cpu.tr = SegmentRegister {
        selector: 0x20,
        base: 0x300,
        limit: 0x67,
        access: 0x8b,
        default_size_32: false,
    };
    let put32 =
        |m: &mut [u8], off: usize, v: u32| m[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put16 =
        |m: &mut [u8], off: usize, v: u16| m[off..off + 2].copy_from_slice(&v.to_le_bytes());
    put32(&mut memory, 0x380 + 32, 0x200); // EIP
    put32(&mut memory, 0x380 + 36, 0x0000_0002); // EFLAGS
    put32(&mut memory, 0x380 + 40, 0xaaaa); // EAX
    put32(&mut memory, 0x380 + 56, 0x00f0); // ESP
    put16(&mut memory, 0x380 + 72, 0x10); // ES
    put16(&mut memory, 0x380 + 76, 0x08); // CS
    put16(&mut memory, 0x380 + 80, 0x10); // SS
    put16(&mut memory, 0x380 + 84, 0x10); // DS
    let mut bus = TestBus::with_memory(memory);

    cpu.cycle(&mut bus).unwrap();

    assert_eq!(cpu.registers.cs().selector, 0x08, "loaded new task CS");
    assert_eq!(cpu.registers.eip, 0x200);
    assert_eq!(cpu.registers.eax(), 0xaaaa);
    assert_eq!(cpu.registers.esp(), 0x00f0);
    assert_eq!(cpu.tr.selector, 0x18, "task register points at the new TSS");
    assert_ne!(cpu.control.cr0 & CR0_TS, 0, "TS set on a task switch");
    // The outgoing task's EIP (past the 5-byte JMP) was saved into the old TSS.
    assert_eq!(
        u32::from_le_bytes(bus.memory[0x320..0x324].try_into().unwrap()),
        5
    );
    // JMP clears the old TSS busy bit in its GDT descriptor (0x8b -> 0x89).
    assert_eq!(bus.memory[0x100 + 0x20 + 5], 0x89);
}

// ---- Phase 1 slice 2 cleanup: BOUND and INS/OUTS ----

#[test]
fn bound_passes_when_in_range() {
    // BOUND AX, [0x100] (62 06 00 01); bounds [10, 20]; AX = 15.
    let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
    memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
    cpu.write_reg16(Reg16::Ax, 15);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eip, 4);
}

#[test]
fn bound_raises_br_out_of_range() {
    let (mut cpu, mut memory) = real_mode_cpu(&[0x62, 0x06, 0x00, 0x01], 0x200);
    memory[0x100..0x102].copy_from_slice(&10u16.to_le_bytes());
    memory[0x102..0x104].copy_from_slice(&20u16.to_le_bytes());
    cpu.write_reg16(Reg16::Ax, 25);
    let mut bus = TestBus::with_memory(memory);
    // BOUND (0x62) is converted to the decode/execute split (task A12); the #BR (vector 5) is
    // raised in `execute_system_seg_decoded`, so run it through the split.
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 5, .. }));
}

#[test]
fn insb_stores_port_byte_to_es_di() {
    // INSB (0x6C): [ES:DI] <- port[DX]. TestBus returns 0, so the 0xFF clears.
    let (mut cpu, mut memory) = real_mode_cpu(&[0x6c], 0x200);
    memory[0x100] = 0xff;
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    cpu.write_reg16(Reg16::Di, 0x0100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(bus.memory[0x100], 0x00);
    assert_eq!(cpu.read_reg16(Reg16::Di), 0x0101);
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoRead && c.address == 0x03f8)
    );
}

#[test]
fn rep_outsw_writes_words_from_ds_si() {
    // REP OUTSW (F3 6F): write CX words from [DS:SI] to port[DX].
    let (mut cpu, memory) = real_mode_cpu(&[0xf3, 0x6f], 0x200);
    cpu.write_reg16(Reg16::Cx, 2);
    cpu.write_reg16(Reg16::Si, 0x0100);
    cpu.write_reg16(Reg16::Dx, 0x03f8);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    let writes = bus
        .trace
        .cycles()
        .iter()
        .filter(|c| {
            c.kind == BusAccessKind::IoWrite && c.width == BusWidth::Word && c.address == 0x03f8
        })
        .count();
    assert_eq!(writes, 2);
    assert_eq!(cpu.read_reg16(Reg16::Cx), 0);
    assert_eq!(cpu.read_reg16(Reg16::Si), 0x0104);
}

// ---- Phase 4 slice E: virtual-8086 mode ----

#[test]
fn v86_segment_load_uses_real_mode_base() {
    // MOV DS, AX (8E D8) in a V86 task: DS base = selector << 4.
    let (mut cpu, memory) = real_mode_cpu(&[0x8e, 0xd8], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags |= FLAG_VM;
    cpu.write_reg16(Reg16::Ax, 0x1234);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).base, 0x1_2340);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x1234);
}

#[test]
fn v86_far_call_uses_real_mode_segments() {
    // CALL FAR 0x8FA9:0x1234 (9A off16 seg16) in a V86 task must be an 8086-style
    // far call (CS = 0x8FA9, base 0x8FA90), never a GDT descriptor lookup — 0x8FA9
    // is not a valid selector and would #GP. Regression for the SP-4b V86 boot:
    // real FreeDOS makes far calls to high segments while virtualized.
    let (mut cpu, memory) = real_mode_cpu(&[0x9a, 0x34, 0x12, 0xa9, 0x8f], 0x200);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.registers.set_esp(0x100);
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.cs().selector, 0x8fa9);
    assert_eq!(cpu.registers.cs().base, 0x8_fa90);
    assert_eq!(cpu.registers.eip & 0xffff, 0x1234);
}

#[test]
fn cli_faults_in_v86_below_iopl3() {
    // CLI (0xFA) in a V86 task with IOPL 0 traps to the monitor with #GP(0).
    // CLI is converted to DecodeGroup::FlagsMisc, so drive it through the split (exec_one_split)
    // rather than execute_instruction_legacy, which no longer carries the 0xFA arm.
    let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM; // IOPL 0
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 13, .. }));
}

#[test]
fn cli_runs_in_v86_at_iopl3() {
    // With IOPL 3 the V86 task may touch IF directly.
    let (mut cpu, memory) = real_mode_cpu(&[0xfa], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000 | FLAG_IF; // IOPL 3, IF set
    let mut bus = TestBus::with_memory(memory);
    cpu.cycle(&mut bus).unwrap();
    assert!(!cpu.flag(FLAG_IF), "CLI cleared IF");
}

#[test]
fn iret_faults_in_v86_below_iopl3() {
    // IRET (0xCF) in a V86 task with IOPL 0 traps to the monitor with #GP(0), exactly
    // like CLI/STI/PUSHF/POPF. This is the TOKAEMM root-cause fix: a V86 guest's IRET
    // must be IOPL-gated so the monitor can virtualize the flags pop (VIF), instead of
    // popping a monitor-stamped IF=0 image straight into real EFLAGS.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM; // IOPL 0
    let esp_before = cpu.registers.esp();
    let mut bus = TestBus::with_memory(memory);
    let fault = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(matches!(fault, InternalFault::Exception { vector: 13, .. }));
    assert_eq!(
        cpu.registers.esp(),
        esp_before,
        "faulted IRET must not pop the stack"
    );
}

#[test]
fn iret_runs_in_v86_at_iopl3() {
    // With IOPL 3 the V86 task may execute a native 8086-style IRET directly: pop
    // IP/CS/FLAGS from the stack with no monitor round-trip.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    // 16-bit IRET frame at SS:0x20 (IP, CS, FLAGS), popped low-to-high.
    bus.memory[0x20..0x22].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.memory[0x22..0x24].copy_from_slice(&0x0050u16.to_le_bytes());
    let popped_flags = (0x2 | FLAG_VM | 0x3000 | FLAG_IF) as u16;
    bus.memory[0x24..0x26].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode(), "IRET at IOPL 3 must stay in V86");
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, 0x0050);
    assert_eq!(cpu.registers.esp(), 0x26, "IRET must pop all three words");
    assert!(
        cpu.flag(FLAG_IF),
        "native IRET loads IF straight from the popped image"
    );
}

#[test]
fn iret_in_v86_at_iopl3_cannot_drop_iopl() {
    // The JEMMEX/TOKAEMM root cause: a V86 client is deliberately run at IOPL 3 so its
    // own native (same-privilege, CPL 3) IRET never traps to the monitor. Per the 386
    // PRM (section 9.7.1.2), "The IOPL field ... is restored only if the CPL is 0" -- at
    // CPL 3 a stale/zeroed IOPL field in the popped image must never reach real EFLAGS.
    let (mut cpu, memory) = real_mode_cpu(&[0xcf], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    bus.memory[0x20..0x22].copy_from_slice(&0x1234u16.to_le_bytes());
    bus.memory[0x22..0x24].copy_from_slice(&0x0050u16.to_le_bytes());
    // Popped image carries IOPL=0 (bits 12-13 clear) -- exactly the stale flags word
    // traced in the field: a JEMM-internal in-V86 IRET popping 0x200.
    let popped_flags = (0x2 | FLAG_VM | FLAG_IF) as u16;
    bus.memory[0x24..0x26].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode());
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        FLAG_IOPL,
        "IRET at CPL 3 must not lower live IOPL from the popped image"
    );
    assert!(
        cpu.flag(FLAG_IF),
        "CPL 3 <= (unchanged) IOPL 3, so IF still loads from the popped image"
    );
}

#[test]
fn popf_in_v86_at_iopl3_cannot_drop_iopl() {
    // Same PRM rule (POPF/POPFD, p.17-136), driven through POPF instead of IRET.
    let (mut cpu, memory) = real_mode_cpu(&[0x9d], 0x40);
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.eflags = 0x2 | FLAG_VM | 0x3000; // IOPL 3
    cpu.cpl = 3; // V86 is always CPL 3; load_flags reads the cached cpl.
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = (0x2 | FLAG_IF) as u16; // IOPL 0 in the popped image
    bus.memory[0x20..0x22].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        FLAG_IOPL,
        "POPF at CPL 3 must not lower live IOPL from the popped image"
    );
    assert!(cpu.is_v86_mode(), "POPF must never clear VM");
}

#[test]
fn pmode_ring3_popf_below_iopl_preserves_if_and_iopl() {
    // Non-V86 ring-3 POPF with IOPL < 3 reaches native load_flags directly (no V86 trap
    // upstream) and per the PRM must leave both IF and IOPL untouched. Built like
    // `cpl3_code`, but with a matching flat CPL-3 SS so POPF can pop the stack.
    let mut memory = vec![0u8; 256];
    memory[0] = 0x9d; // POPF
    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE;
    cpu.registers.set_segment(
        SegmentIndex::Cs,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x9b,
            default_size_32: true,
        },
    );
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x0003,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    cpu.cpl = 3;
    cpu.registers.eflags = 0x2 | FLAG_IF; // IOPL 0, IF set, CPL 3
    cpu.registers.set_esp(0x80);
    let mut bus = TestBus::with_memory(memory);
    // CS.default_size_32 makes plain 9D a POPFD (32-bit pop). Popped image tries to
    // clear IF and raise IOPL to 3.
    let popped_flags = 0x2u32 | 0x3000;
    bus.memory[0x80..0x84].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert!(
        cpu.flag(FLAG_IF),
        "CPL 3 > IOPL 0: IF must keep its live value, not the popped clear"
    );
    assert_eq!(
        cpu.registers.eflags & FLAG_IOPL,
        0,
        "CPL 3 != 0: IOPL must keep its live value, not the popped raise"
    );
}

#[test]
fn cpl0_popfd_still_loads_iopl_and_if_fully() {
    // CPL 0 native POPFD is the one case the PRM lets change IOPL, and IF always loads
    // there too (CPL 0 <= any IOPL). Existing full-load behavior must be unchanged.
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x9d], 0x40); // POPFD
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = 0x2u32 | 0x3000 | FLAG_IF;
    bus.memory[0x20..0x24].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eflags & FLAG_IOPL, FLAG_IOPL);
    assert!(cpu.flag(FLAG_IF));
}

#[test]
fn popfd_can_never_set_vm_at_any_cpl() {
    // POPF/POPFD can never alter VM (bit 17) at any CPL -- real mode here is CPL 0,
    // the most permissive case, and even it must not let VM turn on via a flags pop.
    let (mut cpu, memory) = real_mode_cpu(&[0x66, 0x9d], 0x40); // POPFD
    cpu.registers.set_esp(0x20);
    let mut bus = TestBus::with_memory(memory);
    let popped_flags = 0x2u32 | FLAG_VM;
    bus.memory[0x20..0x24].copy_from_slice(&popped_flags.to_le_bytes());
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(cpu.registers.eflags & FLAG_VM, 0, "POPFD must never set VM");
}

// ---- Stack-group golden battery (A4) ----

/// One golden end-state for a stack-group case, captured from the fused reference
/// (`execute_instruction_legacy`) via `regen_stack_goldens`. Stack ops mutate SS:SP and stack
/// memory, so this captures the full register file (incl. ESP/EBP), eflags (PUSHF/POPF/POPA
/// touch flags), eip, memory-write deltas, and the InstructionPrefetch fetch count.
struct StackGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the stack golden battery. Uses a 512-byte memory image with a stack at
/// 0x1f0 (grows down into the low half) and known register values for non-stack GPRs.
/// The instruction is placed at offset 0; the stack region starts at 0x1f0.
fn stack_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    // AX=0x0102, CX=0x0304, DX=0x0506, BX=0x0708 (non-zero non-trivial values)
    cpu.write_reg16(Reg16::Ax, 0x0102);
    cpu.write_reg16(Reg16::Cx, 0x0304);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Bx, 0x0708);
    // SP=0x01f0, BP=0x01f0 (frame-pointer tests start at the same level)
    cpu.write_reg16(Reg16::Sp, 0x01f0);
    cpu.write_reg16(Reg16::Bp, 0x01f0);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    // eflags: only the always-set reserved bit 1 (PUSHF/POPF tests perturb CF below)
    cpu.registers.eflags = 0x02;
}

/// The stack-group differential battery. Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_stack_goldens`; see `alu_golden_cases` for the
/// full capture recipe. Never edit by hand — re-run the regen from the pre-split commit.
fn stack_golden_cases() -> &'static [StackGolden] {
    &[
        // PUSH reg (0x50-0x57): SP decrements by 2, then value written at ss:SP.
        // Initial SP=0x1f0 so push target is 0x1ee (= 494). The initial 0xBEEF at 0x1f0
        // is unaffected by pushes (they go to 0x1ee = 494).
        StackGolden {
            name: "push ax",
            code: &[80],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(494, 2), (495, 1)],
            fetch: 2,
        },
        StackGolden {
            name: "push bx",
            code: &[83],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(494, 8), (495, 7)],
            fetch: 2,
        },
        StackGolden {
            name: "push cx",
            code: &[81],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(494, 4), (495, 3)],
            fetch: 2,
        },
        StackGolden {
            name: "push si",
            code: &[86],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(494, 8)],
            fetch: 2,
        },
        // POP reg (0x58-0x5f): reads from SS:SP=0x1f0 (BEEF planted there), SP += 2.
        StackGolden {
            name: "pop ax",
            code: &[88],
            gpr: [48879, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "pop bx",
            code: &[91],
            gpr: [258, 772, 1286, 48879, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // PUSH seg (0x06/0x0e/0x16/0x1e): push ES/CS/SS/DS selectors. All are 0 from
        // stack_seed, so no bytes change from initial (they write 0x0000 over 0x0000).
        StackGolden {
            name: "push es",
            code: &[6],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "push cs",
            code: &[14],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "push ss",
            code: &[22],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "push ds",
            code: &[30],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // POP seg (0x07/0x17/0x1f): pops 0xBEEF from stack into ES/SS/DS. No gpr delta
        // (segment selectors are not in `gpr`); SP advances.
        StackGolden {
            name: "pop es",
            code: &[7],
            gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "pop ss",
            code: &[23],
            gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        StackGolden {
            name: "pop ds",
            code: &[31],
            gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // PUSH imm16 (0x68): push 0x1234 to ss:0x1ee.
        StackGolden {
            name: "push imm16 0x1234",
            code: &[104, 52, 18],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(494, 52), (495, 18)],
            fetch: 4,
        },
        // PUSH imm8 +5 (0x6a 0x05): sign-extended to 0x0005; high byte 0x00 over 0x00 = no delta.
        StackGolden {
            name: "push imm8 +5",
            code: &[106, 5],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(494, 5)],
            fetch: 3,
        },
        // PUSH imm8 -1 (0x6a 0xff): sign-extended to 0xffff; both bytes 0xff change.
        StackGolden {
            name: "push imm8 -1",
            code: &[106, 255],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(494, 255), (495, 255)],
            fetch: 3,
        },
        // POP r/m (0x8f /0) memory form: 8F 06 10 01 = POP word [0x0110]. Pops 0xBEEF from
        // ss:0x1f0, writes to ds:0x0110 (= offset 272 dec). SP advances to 0x1f2 (= 498).
        StackGolden {
            name: "pop r/m mem [0x0110]",
            code: &[143, 6, 16, 1],
            gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(272, 239), (273, 190)],
            fetch: 5,
        },
        // POP r/m register form: 8F /0 mod=11 rm=000 -> POP AX. AX gets 0xBEEF.
        StackGolden {
            name: "pop r/m reg ax",
            code: &[143, 192],
            gpr: [48879, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // PUSHA (0x60): snapshot SP=0x1f0 before pushing 8 words. Pushes AX,CX,DX,BX,
        // snapshot-SP,BP,SI,DI. SP ends at 0x1e0 (= 480). The BEEF word at 0x1f0 is
        // overwritten by the SP-snapshot push (0x1ee-0x1ef <- 0x1f0 LE).
        StackGolden {
            name: "pusha",
            code: &[96],
            gpr: [258, 772, 1286, 1800, 480, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[
                (480, 24),
                (482, 8),
                (484, 240),
                (485, 1),
                (486, 240),
                (487, 1),
                (488, 8),
                (489, 7),
                (490, 6),
                (491, 5),
                (492, 4),
                (493, 3),
                (494, 2),
                (495, 1),
            ],
            fetch: 2,
        },
        // POPA (0x61): pops DI,SI,BP,discard,BX,DX,CX,AX from SP=0x1f0. DI gets 0xBEEF
        // (it's the first pop at 0x1f0). All others pop 0x00. SP ends at 0x200 (= 512).
        StackGolden {
            name: "popa",
            code: &[97],
            gpr: [0, 0, 0, 0, 512, 0, 0, 48879],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // PUSHF (0x9c): push eflags (0x0002) to ss:0x1ee. High byte 0x00 over 0x00 = no delta.
        StackGolden {
            name: "pushf",
            code: &[156],
            gpr: [258, 772, 1286, 1800, 494, 496, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(494, 2)],
            fetch: 2,
        },
        // POPF (0x9d): pops 0x0097 from ss:0x1f0 (overridden from BEEF in the test loop).
        // CF+PF+AF+ZF+SF all set. SP advances to 0x1f2 (= 498).
        StackGolden {
            name: "popf",
            code: &[157],
            gpr: [258, 772, 1286, 1800, 498, 496, 8, 24],
            eflags: 0x97,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // ENTER imm16=4, imm8=1 (nesting level 1): push BP (0x01f0), copy frame ptr, set
        // BP = pre-push SP - 2, then SP -= alloc (4). Stack frame consumes 4 bytes (2 for
        // saved BP, 2 for the display copy). SP ends at 0x1e8 (= 488); BP=0x1ee (= 494).
        StackGolden {
            name: "enter 4,1",
            code: &[200, 4, 0, 1],
            gpr: [258, 772, 1286, 1800, 488, 494, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(492, 238), (493, 1), (494, 240), (495, 1)],
            fetch: 5,
        },
        // LEAVE (0xc9): SP <- BP = 0x1f0, then pop BP from ss:0x1f0 (BEEF). BP = 0xBEEF,
        // SP = 0x1f2 (= 498).
        StackGolden {
            name: "leave",
            code: &[201],
            gpr: [258, 772, 1286, 1800, 498, 48879, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
    ]
}

/// Seed memory for the stack golden battery. Plants 0xBEEF at SS:SP=0x1f0 (the first
/// word a POP reads) so POP tests have a stable, visible source. Each case gets a fresh
/// 0x200-byte vector so earlier writes don't bleed into later cases. The POPF case
/// overwrites this with 0x0097 in the regen/assert loops to give CF+PF+AF+ZF+SF.
fn stack_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // POP tests: plant 0xBEEF at ss:0x1f0 (the initial SP — the first word a POP reads).
    mem[0x1f0..0x1f2].copy_from_slice(&0xbeefu16.to_le_bytes());
}

#[test]
fn stack_split_matches_golden_across_ops() {
    // The stack-group opcodes (PUSH/POP reg/seg/imm, PUSHA/POPA, PUSHF/POPF, ENTER/LEAVE,
    // POP r/m) are converted to the decode/execute split, so they can no longer be diffed
    // against a fused executor (that path was deleted). Run each through cycle() and assert
    // the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_stack_goldens`. Covers register and memory operands, SP semantics, flag
    // masking (PUSHF/POPF), the PUSHA SP-snapshot, and the ENTER nesting frame-copy.
    for g in stack_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        stack_seed_mem(&mut mem, g.code);
        // POPF needs a known flags word at SS:SP (0x1f0) instead of BEEF.
        if g.name == "popf" {
            mem[0x1f0..0x1f2].copy_from_slice(&0x0097u16.to_le_bytes());
        }
        let initial = mem.clone();

        let mut split = Cpu386::default();
        stack_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `stack_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the stack group's fused arms still exist in `dispatch_opcode`:
///   cargo test -p izarravm-cpu --lib regen_stack_goldens -- --ignored --nocapture
/// then paste the output over `stack_golden_cases` and only then do the conversion.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_stack_goldens() {
    for g in stack_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        stack_seed_mem(&mut mem, g.code);
        if g.name == "popf" {
            mem[0x1f0..0x1f2].copy_from_slice(&0x0097u16.to_le_bytes());
        }
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        stack_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting fused arms",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            StackGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// One golden end-state for an arithmetic /ext group case (groups 1-4), captured the same way
/// as the other group goldens: opcode bytes plus expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI),
/// eflags, eip, (offset,value) memory writes, and InstructionPrefetch fetch count. Groups 1-3
/// touch flags (ALU/shift/TEST/NEG/MUL/DIV), so eflags is load-bearing; group 4 (INC/DEC) must
/// leave CF untouched, which the CF-preserving seed makes visible in eflags.
struct GroupGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the group golden battery: the same register file as `seam_seed` plus CL=4 (a small
/// shift count) and CF pre-set in eflags so the INC/DEC CF-preservation is observable. CX is
/// 0x0304 so CL = 0x04.
fn group_seed(cpu: &mut Cpu386) {
    seam_seed(cpu);
    // Pre-set CF (bit 0) on top of the always-set reserved bit 1. This makes the group 4
    // INC/DEC CF-preservation visible (CF must still be set after) and feeds ADC/SBB/RCR.
    cpu.registers.eflags = 0x03;
}

/// Seed memory for the group battery: plant 0x3412 at [bx] = ds:0x10 (the r/m memory target),
/// so byte [0x10] = 0x12 and word [0x10] = 0x3412. Fresh image per case so writes don't bleed.
fn group_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x10..0x12].copy_from_slice(&0x3412u16.to_le_bytes());
}

/// The arithmetic /ext group (1-4) differential battery. Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_group_goldens`; see `alu_golden_cases` for the full
/// capture recipe. Never edit by hand — re-run the regen WHILE the fused arms still exist in
/// `dispatch_opcode`, then paste, then delete the fused arms. Covers: group 1 ALU r/m,imm
/// (byte/word, CMP no-writeback, 0x83 sign-extend), group 2 shift/rotate (SHL/SHR/SAR/ROL/RCR
/// with count 1/CL/imm8), group 3 TEST-with-imm/NOT/NEG/MUL/IMUL and a non-faulting DIV, and
/// group 4 INC/DEC (CF preserved). The DIV-by-zero #DE fault is a separate test (goldens only
/// capture success).
fn group_golden_cases() -> &'static [GroupGolden] {
    &[
        // Group 1: ALU r/m, imm (0x80/0x81/0x82/0x83). Includes CMP no-writeback and 0x83
        // sign-extend (both byte/word and a register form).
        GroupGolden {
            name: "add byte [bx],0x05 (80 /0)",
            code: &[128, 7, 5],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x6,
            eip: 0x3,
            deltas: &[(16, 23)],
            fetch: 4,
        },
        GroupGolden {
            name: "or byte [bx],0xf0 (80 /1)",
            code: &[128, 15, 240],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x82,
            eip: 0x3,
            deltas: &[(16, 242)],
            fetch: 4,
        },
        GroupGolden {
            name: "cmp byte [bx],0x12 (80 /7 no writeback)",
            code: &[128, 63, 18],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        GroupGolden {
            name: "add word [bx],0x1234 (81 /0)",
            code: &[129, 7, 52, 18],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(16, 70), (17, 70)],
            fetch: 5,
        },
        GroupGolden {
            name: "cmp word [bx],0x3412 (81 /7 no writeback)",
            code: &[129, 63, 18, 52],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        GroupGolden {
            name: "add word [bx],-2 (83 /0 sign-extend)",
            code: &[131, 7, 254],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x13,
            eip: 0x3,
            deltas: &[(16, 16)],
            fetch: 4,
        },
        GroupGolden {
            name: "sub ax,-1 (83 /5 sign-extend reg)",
            code: &[131, 232, 255],
            gpr: [259, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x17,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // Group 2: shift/rotate (0xc0/0xc1/0xd0-0xd3). Flags load-bearing; count 1/CL/imm8.
        GroupGolden {
            name: "shl byte [bx],1 (d0 /4)",
            code: &[208, 39],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x6,
            eip: 0x2,
            deltas: &[(16, 36)],
            fetch: 3,
        },
        GroupGolden {
            name: "shr word [bx],1 (d1 /5)",
            code: &[209, 47],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x6,
            eip: 0x2,
            deltas: &[(16, 9), (17, 26)],
            fetch: 3,
        },
        GroupGolden {
            name: "shl ax,1 (d1 /4 reg)",
            code: &[209, 224],
            gpr: [516, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        GroupGolden {
            name: "rol byte [bx],cl (d2 /0)",
            code: &[210, 7],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x2,
            deltas: &[(16, 33)],
            fetch: 3,
        },
        GroupGolden {
            name: "sar word [bx],cl (d3 /7)",
            code: &[211, 63],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x6,
            eip: 0x2,
            deltas: &[(16, 65), (17, 3)],
            fetch: 3,
        },
        GroupGolden {
            name: "rcr word [bx],3 (c1 /3 imm8)",
            code: &[193, 31, 3],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[(16, 130), (17, 166)],
            fetch: 4,
        },
        GroupGolden {
            name: "shl ax,4 (c1 /4 imm8 reg)",
            code: &[193, 224, 4],
            gpr: [4128, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // Group 3: F6/F7 (TEST-with-imm/NOT/NEG/MUL/IMUL/DIV). DIV here is non-faulting; the
        // DIV-by-zero #DE is covered by `group_div_by_zero_raises_de_through_the_split`.
        GroupGolden {
            name: "test byte [bx],0x0f (f6 /0 imm)",
            code: &[246, 7, 15],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        GroupGolden {
            name: "test ax,0x00ff (f7 /0 imm reg)",
            code: &[247, 192, 255, 0],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        GroupGolden {
            name: "not word [bx] (f7 /2)",
            code: &[247, 23],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x2,
            deltas: &[(16, 237), (17, 203)],
            fetch: 3,
        },
        GroupGolden {
            name: "neg ax (f7 /3 reg)",
            code: &[247, 216],
            gpr: [65278, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x93,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        GroupGolden {
            name: "mul bl (f6 /4 reg)",
            code: &[246, 227],
            gpr: [32, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        GroupGolden {
            name: "imul cx (f7 /5 reg)",
            code: &[247, 233],
            gpr: [2568, 772, 3, 16, 0, 16, 8, 24],
            eflags: 0x803,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        GroupGolden {
            name: "div bl (f6 /6 reg, non-faulting)",
            code: &[246, 243],
            gpr: [528, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // Group 4: INC/DEC byte (0xfe). CF must be preserved (the seed pre-sets CF; both end
        // states keep bit 0 set).
        GroupGolden {
            name: "inc byte [bx] (fe /0, CF preserved)",
            code: &[254, 7],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x2,
            deltas: &[(16, 19)],
            fetch: 3,
        },
        GroupGolden {
            name: "dec byte [bx] (fe /1, CF preserved)",
            code: &[254, 15],
            gpr: [258, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x7,
            eip: 0x2,
            deltas: &[(16, 17)],
            fetch: 3,
        },
    ]
}

#[test]
fn group_split_matches_golden_across_ops() {
    // The arithmetic /ext groups 1-4 (ALU r/m,imm; shift/rotate; TEST/NOT/NEG/MUL/IMUL/DIV/IDIV;
    // INC/DEC) are converted to the decode/execute split, so they can no longer be diffed
    // against a fused executor (those arms were deleted). Run each through cycle() and assert
    // the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_group_goldens`. Exercises decode's ModRM/addressing parse, the conditional F6/F7
    // immediate, the executor's sub-op dispatch + write-back gating (CMP/TEST flags-only), the
    // reused shift/mul/div flag logic, CF preservation on INC/DEC, and the once-only fetch
    // charge. The DIV-by-zero #DE fault is covered separately (goldens capture success only).
    for g in group_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        group_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        group_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `group_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the group's fused arms (0x80-0x83, 0xc0/0xc1/0xd0-0xd3, 0xf6/0xf7, 0xfe) still
/// exist in `dispatch_opcode`:
///   cargo test -p izarravm-cpu --lib regen_group_goldens -- --ignored --nocapture
/// then paste the output over `group_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_group_goldens() {
    for g in group_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        group_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        group_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run before deleting fused arms",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            GroupGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

#[test]
fn group_div_by_zero_raises_de_through_the_split() {
    // The DIV-by-zero #DE fault path (goldens capture success only, so it needs an explicit
    // test). `div bl` (F6 /6, mod=11 rm=011) with BL = 0 must raise the divide error. The
    // group 3 fused arm is deleted on this branch, so this drives the decode/execute split
    // (exec_one_split) directly and asserts the raw fault is the deliverable
    // `InternalFault::Exception { vector: 0, .. }` (#DE, no error code) -- `exec_one_split`
    // runs below `finish_instruction`/`deliver_exception`, so this checks the raise site
    // itself, not the delivered frame. The `div` helper checks divide-by-zero BEFORE any
    // register write, and `decode` consumes exactly the F6 + ModRM bytes (no immediate for
    // /6), so we also assert eip advanced by 2. The InstructionPrefetch count (3, one
    // read-ahead past the 2-byte op — see the non-faulting `div bl` golden, which also
    // reports 3) confirms decode charged the fetch and the executor faulted with no extra
    // fetch.
    let code = [0xf6, 0xf3]; // div bl
    let mut mem = vec![0u8; 0x40];
    mem[..code.len()].copy_from_slice(&code);

    let mut split = Cpu386::default();
    split.load_segment_real(SegmentIndex::Cs, 0);
    split.load_segment_real(SegmentIndex::Ds, 0);
    split.registers.eip = 0;
    split.write_reg16(Reg16::Ax, 0x0102);
    split.write_reg16(Reg16::Bx, 0x0700); // BL = 0 -> divide by zero
    let mut sbus = TestBus::with_memory(mem);
    let split_err = exec_one_split(&mut split, &mut sbus).unwrap_err();

    assert!(
        matches!(
            split_err,
            InternalFault::Exception {
                vector: 0,
                error_code: None
            }
        ),
        "split DIV-by-zero must raise a deliverable #DE, got {split_err:?}"
    );
    // AX must be untouched: the #DE is raised before any quotient/remainder write-back.
    assert_eq!(
        split.read_reg16(Reg16::Ax),
        0x0102,
        "AX must be unchanged when DIV faults before write-back"
    );
    assert_eq!(
        split.registers.eip, 2,
        "decode must consume the F6 + ModRM bytes (no immediate for /6) before the #DE"
    );
    assert_eq!(
        seam_fetch_count(&sbus),
        3,
        "the split must charge the same fetches as the non-faulting div bl golden (3)"
    );
}

/// One golden end-state for a relative/loop branch case (task A6a). Adds `cx` to the shared
/// golden shape so a single battery can drive both the taken and not-taken LOOP/JCXZ/LOOPcc
/// outcomes (which differ only in the post-decrement count) from one seed — `branch_seed`
/// overwrites CX with this per-case value before the instruction runs. The captured fields are
/// the standard set: end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip (the branch target — the key
/// assertion for this group), (offset,value) memory writes (CALL's pushed return address), and
/// the InstructionPrefetch fetch count.
struct BranchGolden {
    name: &'static str,
    code: &'static [u8],
    cx: u32,
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the branch golden battery. CS/DS/SS = 0, eip = 0, SP = 0x100 (a safe in-image stack
/// so CALL's push lands in the 0x200-byte image), ZF pre-set (so the Jcc/LOOPcc condition cases
/// are deterministic), and CX set per case (the caller overwrites it from `BranchGolden::cx`).
fn branch_seed(cpu: &mut Cpu386, cx: u32) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.set_flag(FLAG_ZF, true);
    cpu.registers.set_ecx(cx);
}

/// The relative/loop branch differential battery. Captured from the PRIOR fused reference via
/// `regen_branch_goldens`; see `alu_golden_cases` for the full capture recipe. The branch group's
/// fused arms (0x70-0x7f, 0xe0-0xe3, 0xe8/0xe9/0xeb, 0F 80-0F 8F) are already deleted on
/// `perf-decode-cache`, so these were captured from the pre-split base commit (a94ed279): check
/// it out, run the regen, paste, return. Never hand-edit a golden — re-capture from the reference.
/// Covers: Jcc short taken/not-taken (JZ/JNZ with ZF set), Jcc near (two-byte) taken/not-taken,
/// JMP short, JMP near, CALL near (the pushed return address + SP delta), LOOP taken (CX
/// decremented, nonzero) and not-taken (CX hits 0), LOOPE/LOOPNE (ZF interaction), and JCXZ
/// taken (CX==0) / not-taken (CX!=0).
fn branch_golden_cases() -> &'static [BranchGolden] {
    &[
        // Jcc short (rel8). ZF is pre-set, so JZ is taken and JNZ falls through.
        BranchGolden {
            name: "jz +5 taken (74, ZF set)",
            code: &[0x74, 0x05],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x7,
            deltas: &[],
            fetch: 3,
        },
        BranchGolden {
            name: "jnz +5 not taken (75, ZF set)",
            code: &[0x75, 0x05],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // Jcc short with a backward (negative) rel8 — exercises the sign-extension.
        BranchGolden {
            name: "jz -2 taken backward (74, ZF set)",
            code: &[0x74, 0xfe],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x0,
            deltas: &[],
            fetch: 3,
        },
        // Jcc near, two-byte (rel16). ZF pre-set: 0F 84 taken, 0F 85 falls through.
        BranchGolden {
            name: "jz near +0x100 taken (0F 84, ZF set)",
            code: &[0x0f, 0x84, 0x00, 0x01],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x104,
            deltas: &[],
            fetch: 5,
        },
        BranchGolden {
            name: "jnz near +0x100 not taken (0F 85, ZF set)",
            code: &[0x0f, 0x85, 0x00, 0x01],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // JMP short (rel8) and JMP near (rel16): unconditional.
        BranchGolden {
            name: "jmp short +5 (eb)",
            code: &[0xeb, 0x05],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x7,
            deltas: &[],
            fetch: 3,
        },
        BranchGolden {
            name: "jmp near +0x100 (e9)",
            code: &[0xe9, 0x00, 0x01],
            cx: 3,
            gpr: [0, 3, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x103,
            deltas: &[],
            fetch: 4,
        },
        // CALL near (rel16): push the return address (post-instruction eip = 3) then branch.
        // SP drops by 2 (0x100 -> 0xfe) and [SS:0xfe] holds the little-endian return address.
        BranchGolden {
            name: "call near +0x100 (e8, push return)",
            code: &[0xe8, 0x00, 0x01],
            cx: 3,
            gpr: [0, 3, 0, 0, 254, 0, 0, 0],
            eflags: 0x42,
            eip: 0x103,
            deltas: &[(0xfe, 0x03)],
            fetch: 4,
        },
        // LOOP (0xe2): decrement CX, branch while nonzero.
        BranchGolden {
            name: "loop +5 taken (e2, cx 3->2)",
            code: &[0xe2, 0x05],
            cx: 3,
            gpr: [0, 2, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x7,
            deltas: &[],
            fetch: 3,
        },
        BranchGolden {
            name: "loop +5 not taken (e2, cx 1->0)",
            code: &[0xe2, 0x05],
            cx: 1,
            gpr: [0, 0, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // LOOPE (0xe1, loops while ZF=1) and LOOPNE (0xe0, loops while ZF=0). ZF pre-set.
        BranchGolden {
            name: "loope +5 taken (e1, ZF set, cx 3->2)",
            code: &[0xe1, 0x05],
            cx: 3,
            gpr: [0, 2, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x7,
            deltas: &[],
            fetch: 3,
        },
        BranchGolden {
            name: "loopne +5 not taken (e0, ZF set, cx 3->2)",
            code: &[0xe0, 0x05],
            cx: 3,
            gpr: [0, 2, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // JCXZ (0xe3): branch when CX == 0, no decrement.
        BranchGolden {
            name: "jcxz +5 taken (e3, cx==0)",
            code: &[0xe3, 0x05],
            cx: 0,
            gpr: [0, 0, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x7,
            deltas: &[],
            fetch: 3,
        },
        BranchGolden {
            name: "jcxz +5 not taken (e3, cx!=0)",
            code: &[0xe3, 0x05],
            cx: 1,
            gpr: [0, 1, 0, 0, 256, 0, 0, 0],
            eflags: 0x42,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
    ]
}

#[test]
fn branch_split_matches_golden_across_ops() {
    // The relative/loop branch block (Jcc short/near, JMP short/near, CALL near, LOOP/LOOPE/
    // LOOPNE/JCXZ) is converted to the decode/execute split, so its fused arms are deleted and it
    // can no longer be diffed against a fused executor. Run each case through cycle() (the split)
    // and assert the architectural end-state against goldens captured from the pre-split fused
    // path via `regen_branch_goldens`. The eip field is the load-bearing assertion: it is the
    // branch target, proving decode stored the right sign-extended displacement and the executor
    // reproduced the fused eip-relative math (rel8 vs rel16, taken vs fall-through). CALL also
    // asserts the pushed return address (memory delta) and the SP decrement (gpr[4]).
    for g in branch_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        branch_seed(&mut split, g.cx);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `branch_golden_cases` from the fused reference. Ignored by default.
/// The branch fused arms are already deleted on `perf-decode-cache`, so run this from the
/// pre-split base commit (a94ed279) where they still exist:
///   git stash && git checkout a94ed279
///   cargo test -p izarravm-cpu --lib regen_branch_goldens -- --ignored --nocapture
/// then paste the output over `branch_golden_cases`, return to the branch, and only then trust it.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_branch_goldens() {
    for g in branch_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        branch_seed(&mut fused, g.cx);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            BranchGolden {{ name: {:?}, code: &{:?}, cx: {}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            g.cx,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// One golden end-state for a control-flow case (task A6b). Mirrors the `BranchGolden` shape but
/// adds `cs` (the CS selector) and a per-case `setup` closure, because this group changes
/// segment state (RETF, far-direct CALL/JMP, and the INT/IRET deliveries reload CS) and each
/// form needs its own in-memory image (a far pointer / IVT entry / saved stack frame). The
/// captured fields are the standard set plus `cs`: end gpr (AX,CX,DX,BX,SP,BP,SI,DI), the CS
/// selector, eflags, eip, (offset,value) memory writes (CALL/PUSH/INT push; INC/DEC write), and
/// the InstructionPrefetch fetch count.
struct ControlFlowGolden {
    name: &'static str,
    code: &'static [u8],
    /// Per-case memory image written before the run (IVT entries, far pointers, saved frames),
    /// applied identically on the split and the fused-reference paths.
    setup: fn(&mut [u8]),
    gpr: [u32; 8],
    cs: u16,
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Shared register seed for the control-flow golden battery: CS/DS/SS = 0, eip = 0, SP = 0x100
/// (a safe in-image stack), BX = 0x40 (so `[bx]` addresses the in-image FF r/m operand), and the
/// OF/IF flags set so INTO traps and the interrupt deliveries record IF being cleared. The
/// per-case `setup` closure lays down the memory image each form needs.
fn controlflow_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x0100);
    cpu.write_reg16(Reg16::Bx, 0x0040);
    cpu.set_flag(FLAG_OF, true);
    cpu.set_flag(FLAG_IF, true);
}

/// The far/indirect/RET/INT control-flow + 0xff group-5 differential battery. Captured from the
/// PRIOR fused reference via `regen_controlflow_goldens`; see `branch_golden_cases` for the
/// capture recipe. These opcodes' fused arms are deleted on `perf-decode-cache`, so the goldens
/// were captured from the pre-split base commit (HEAD before A6b, dc1cf4e2): the regen runs the
/// fused `execute_instruction_legacy` there, prints the literals, and they are pasted back.
///
/// Covers the non-faulting success paths: RET near (with and without an imm16 SP-release), RETF
/// (the CS reload + SP delta), FF /0 INC and FF /1 DEC r/m (the memory write + the flag update),
/// FF /6 PUSH r/m (the pushed value + SP drop), FF /2 near-indirect CALL (the pushed return +
/// the new eip), FF /4 near-indirect JMP (the new eip, nothing pushed), CALL/JMP far direct
/// (0x9a/0xea — the CS:eip transfer, plus CALL's pushed CS:IP), and the INT3/INT n/INTO/IRET
/// deliveries (CS:eip from the IVT, the pushed FLAGS:CS:IP frame / the restored frame, IF
/// cleared). The shared `controlflow_seed` plus each case's `setup` makes every input stable.
fn controlflow_golden_cases() -> &'static [ControlFlowGolden] {
    &[
        ControlFlowGolden {
            name: "ret near (c3, pop 0x0100)",
            code: &[0xc3],
            setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 258, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "ret near imm16 (c2 04 00, pop then release 4)",
            code: &[0xc2, 0x04, 0x00],
            setup: |m| m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 262, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 4,
        },
        ControlFlowGolden {
            name: "retf (cb, pop 0x0100:0x3000)",
            code: &[0xcb],
            setup: |m| {
                m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                m[0x102..0x104].copy_from_slice(&0x3000u16.to_le_bytes());
            },
            gpr: [0, 0, 0, 64, 260, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "ff /0 inc word [bx] (0x0080 -> 0x0081)",
            code: &[0xff, 0x07],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0x206,
            eip: 0x2,
            deltas: &[(64, 129)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /1 dec word [bx] (0x0080 -> 0x007f)",
            code: &[0xff, 0x0f],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0x212,
            eip: 0x2,
            deltas: &[(64, 127)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /6 push word [bx] (push 0x0080)",
            code: &[0xff, 0x37],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 254, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x2,
            deltas: &[(254, 128)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /2 call near [bx] (push return 2, jump 0x0080)",
            code: &[0xff, 0x17],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 254, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x80,
            deltas: &[(254, 2)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "ff /4 jmp near [bx] (jump 0x0080, nothing pushed)",
            code: &[0xff, 0x27],
            setup: |m| m[0x40..0x42].copy_from_slice(&0x0080u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x0,
            eflags: 0xa02,
            eip: 0x80,
            deltas: &[],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "call far 0x3000:0x0100 (9a, push cs:ip)",
            code: &[0x9a, 0x00, 0x01, 0x00, 0x30],
            setup: |_m| {},
            gpr: [0, 0, 0, 64, 252, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[(252, 5)],
            fetch: 6,
        },
        ControlFlowGolden {
            name: "jmp far 0x3000:0x0100 (ea, nothing pushed)",
            code: &[0xea, 0x00, 0x01, 0x00, 0x30],
            setup: |_m| {},
            gpr: [0, 0, 0, 64, 256, 0, 0, 0],
            cs: 0x3000,
            eflags: 0xa02,
            eip: 0x100,
            deltas: &[],
            fetch: 6,
        },
        ControlFlowGolden {
            name: "int3 (cc, ivt[3] -> 0000:0040)",
            code: &[0xcc],
            setup: |m| m[12..14].copy_from_slice(&0x0040u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x40,
            deltas: &[(250, 1), (254, 2), (255, 10)],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "int 0x21 (cd 21, ivt[0x21] -> 0000:0050)",
            code: &[0xcd, 0x21],
            setup: |m| m[0x84..0x86].copy_from_slice(&0x0050u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x50,
            deltas: &[(250, 2), (254, 2), (255, 10)],
            fetch: 3,
        },
        ControlFlowGolden {
            name: "into with OF set (ce, ivt[4] -> 0000:0060)",
            code: &[0xce],
            setup: |m| m[16..18].copy_from_slice(&0x0060u16.to_le_bytes()),
            gpr: [0, 0, 0, 64, 250, 0, 0, 0],
            cs: 0x0,
            eflags: 0x802,
            eip: 0x60,
            deltas: &[(250, 1), (254, 2), (255, 10)],
            fetch: 2,
        },
        ControlFlowGolden {
            name: "iret (cf, restore 0000:0100 flags 0x0202)",
            code: &[0xcf],
            setup: |m| {
                m[0x100..0x102].copy_from_slice(&0x0100u16.to_le_bytes());
                m[0x102..0x104].copy_from_slice(&0x0000u16.to_le_bytes());
                m[0x104..0x106].copy_from_slice(&0x0202u16.to_le_bytes());
            },
            gpr: [0, 0, 0, 64, 262, 0, 0, 0],
            cs: 0x0,
            eflags: 0x202,
            eip: 0x100,
            deltas: &[],
            fetch: 2,
        },
    ]
}

#[test]
fn controlflow_split_matches_golden_across_ops() {
    // The far/indirect/RET/INT control-flow block + 0xff group 5 is converted to the decode/
    // execute split, so its fused arms are deleted and it can no longer be diffed against a fused
    // executor in-tree. Run each case through cycle() (the split) and assert the architectural
    // end-state against goldens captured from the pre-split fused path via
    // `regen_controlflow_goldens`. eip is the branch/return/vector target; cs proves RETF / the
    // far-direct / INT deliveries reloaded the segment; the memory deltas prove CALL/PUSH/INT
    // pushed (and INC/DEC wrote) the right bytes; the fetch count proves decode charged each
    // instruction byte exactly once.
    for g in controlflow_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        controlflow_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        split.cycle(&mut sbus).unwrap();

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(
            split.registers.cs().selector,
            g.cs,
            "cs mismatch for {}",
            g.name
        );
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `controlflow_golden_cases` from the fused reference. Ignored by default. The
/// control-flow fused arms are already deleted on `perf-decode-cache`, so run this from the
/// pre-split base commit (HEAD before A6b, dc1cf4e2) where they still exist:
///   git worktree add ../regen dc1cf4e2 && cd ../regen
///   cargo test -p izarravm-cpu --lib regen_controlflow_goldens -- --ignored --nocapture
/// then paste the output over `controlflow_golden_cases`, return to the branch, and only then
/// trust it. (Copy this test body + the struct/seed into the throwaway worktree if the fused
/// base predates them.)
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_controlflow_goldens() {
    for g in controlflow_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        controlflow_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            // {}\n            gpr: {:?}, cs: {:#x}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {},",
            g.name,
            fused.registers.gpr,
            fused.registers.cs().selector,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

/// FF /7 is an undefined group-5 encoding and must raise the group-opcode error (which the
/// emulator maps to #UD), not silently execute. Drive it through the split and assert the error.
#[test]
fn controlflow_ff_ext7_is_undefined() {
    // 0xff 0x3f: mod=00 reg=111 rm=111 -> group 5 /7 with a memory r/m. The /7 extension is
    // undefined regardless of the addressing form.
    let (mut cpu, memory) = real_mode_cpu(&[0xff, 0x3f], 0x100);
    let mut bus = TestBus::with_memory(memory);
    let err = exec_one_split(&mut cpu, &mut bus).unwrap_err();
    assert!(
        matches!(
            err,
            InternalFault::Exception {
                vector: 6,
                error_code: None
            }
        ),
        "FF /7 must raise a deliverable #UD, got {err:?}"
    );
}

// ---- Flags + misc register golden battery (A7) ----

/// One golden end-state for a flags/misc register case (task A7). The standard shape:
/// opcode bytes, expected end gpr (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, and the
/// InstructionPrefetch fetch count. No memory writes (none of the A7 opcodes write to memory),
/// so no `deltas` field. The `eflags` field is load-bearing for most cases (TEST/SAHF/CLC/STC/
/// CMC/CLD/STD/CLI/STI change flags; INC/DEC change S/Z/O/A/P while preserving CF; CBW/CWD
/// change registers only). `eip` advances past the instruction (1 byte for all except TEST
/// 0x84/0x85 which have a ModRM, so 2 bytes).
struct FlagsMiscGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    fetch: usize,
}

/// Seed for the flags/misc golden battery: the same register file as `seam_seed` plus CF
/// pre-set (so INC/DEC CF-preservation is visible and CMC/CLC/STC have a known starting CF),
/// and AH=0xd7 (= 0b11010111: CF/PF/AF/ZF/SF all 1, bits 3/5 forced — so LAHF/SAHF transfer
/// a non-trivial value). AH lives in the high byte of AX; write_gpr8(4, 0xd7) sets it.
fn flags_misc_seed(cpu: &mut Cpu386) {
    seam_seed(cpu);
    // CF set (bit 0 on top of always-1 bit 1). Makes INC/DEC CF-preservation observable and
    // gives CMC/CLC/STC a known starting state.
    cpu.registers.eflags = 0x03;
    // AH = 0xd7 (bit pattern: CF=1, PF=1, AF=1, ZF=1, SF=1, reserved bits 1/3/5).
    // This is the value SAHF loads into the low flag byte, and LAHF reads it back out.
    cpu.write_gpr8(4, 0xd7);
}

/// Seed memory for the flags/misc battery: plant a word at [bx]=ds:0x10 (the TEST r/m target).
fn flags_misc_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // TEST byte [bx]: [0x10] = 0x12; TEST word [bx]: [0x10..0x12] = 0x3412.
    mem[0x10..0x12].copy_from_slice(&0x3412u16.to_le_bytes());
}

/// The flags + misc register differential battery (task A7). Captured from the PRIOR fused
/// reference (`execute_instruction_legacy`) via `regen_flags_misc_goldens`; see
/// `alu_golden_cases` for the full capture recipe. Never edit by hand — re-run the regen WHILE
/// the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd) still exist
/// in `dispatch_opcode`, then paste, then delete the fused arms. Covers: TEST byte/word reg and
/// mem (flags set, no write-back); INC/DEC reg (CF preserved, overflow and sign visible); CBW/
/// CWDE/CWD/CDQ (operand-size-dependent sign extension); SAHF/LAHF (flag-byte round-trip); and
/// all seven flag-bit ops CMC/CLC/STC/CLI/STI/CLD/STD (correct bit set/clear/complement;
/// STI interrupt shadow is covered by a dedicated test).
fn flags_misc_golden_cases() -> &'static [FlagsMiscGolden] {
    // Captured from the fused reference (`execute_instruction_legacy`) via
    // `regen_flags_misc_goldens` run against parent commit 3912fbc5.
    // Seed: AX=0xD702 (AH=0xd7, AL=0x02; seam_seed sets AX=0x0102 then AH=0xd7),
    // CX=0x0304, DX=0x0506, BX=0x0010, SP=0, BP=0x0010, SI=0x0008, DI=0x0018.
    // eflags=0x03 (CF=1, always-1 bit1=1). Memory: [0x10..0x12] = 0x3412.
    &[
        // TEST r/m8,reg8 (0x84): flags only, no write-back. TEST AL,AL: 0x02 AND 0x02 = 0x02,
        // ZF=0 PF=0 SF=0 CF=0 OF=0 → eflags=0x02 (reserved bit only).
        FlagsMiscGolden {
            name: "test al,al (84 c0)",
            code: &[0x84, 0xc0],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m8,reg8 (0x84): TEST [bx],cl: [0x10]=0x12 AND CL=0x04 → 0x00, ZF=1 → 0x46.
        FlagsMiscGolden {
            name: "test [bx],cl (84 0f)",
            code: &[0x84, 0x0f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m16,reg16 (0x85): TEST BX,CX: 0x0010 AND 0x0304 = 0x0000, ZF=1 PF=1 → 0x46.
        FlagsMiscGolden {
            name: "test bx,cx (85 cb)",
            code: &[0x85, 0xcb],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // TEST r/m16,reg16 (0x85): TEST [bx],cx: [0x10]=0x3412 AND 0x0304 = 0x0000, ZF=1 → 0x46.
        FlagsMiscGolden {
            name: "test [bx],cx (85 0f)",
            code: &[0x85, 0x0f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x46,
            eip: 0x2,
            fetch: 3,
        },
        // INC AX (0x40): AX=0xd702 → 0xd703. CF preserved (stays 1). AF set (low nibble 2→3).
        FlagsMiscGolden {
            name: "inc ax (40)",
            code: &[0x40],
            gpr: [55043, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x87,
            eip: 0x1,
            fetch: 2,
        },
        // INC DI (0x47): DI=0x0018 → 0x0019. CF preserved (stays 1). No half-carry.
        FlagsMiscGolden {
            name: "inc di (47)",
            code: &[0x47],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 25],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // DEC AX (0x48): AX=0xd702 → 0xd701. CF preserved (stays 1). SF set (high bit of AH).
        FlagsMiscGolden {
            name: "dec ax (48)",
            code: &[0x48],
            gpr: [55041, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x83,
            eip: 0x1,
            fetch: 2,
        },
        // DEC DI (0x4f): DI=0x0018 → 0x0017. CF preserved (stays 1). AF set.
        FlagsMiscGolden {
            name: "dec di (4f)",
            code: &[0x4f],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 23],
            eflags: 0x7,
            eip: 0x1,
            fetch: 2,
        },
        // CBW (0x98): sign-extend AL=0x02 (positive) → AX=0x0002. AH cleared.
        FlagsMiscGolden {
            name: "cbw (98, al=0x02)",
            code: &[0x98],
            gpr: [2, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CWD (0x99): AX=0xd702 (sign bit set; 0xd702 as i16 = -10494 < 0) → DX=0xFFFF.
        FlagsMiscGolden {
            name: "cwd (99, ax positive)",
            code: &[0x99],
            gpr: [55042, 772, 65535, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // SAHF (0x9e): AH=0xd7 (= 1101_0111b) → flags low byte = d7 (CF=1 PF=1 AF=1 ZF=1 SF=1).
        FlagsMiscGolden {
            name: "sahf (9e, ah=0xd7)",
            code: &[0x9e],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0xd7,
            eip: 0x1,
            fetch: 2,
        },
        // LAHF (0x9f): eflags=0x03 → AH = (0x03 & 0xD5) | 0x02 = 0x03. AX = 0x0302=770.
        FlagsMiscGolden {
            name: "lahf (9f, eflags=0x03)",
            code: &[0x9f],
            gpr: [770, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CMC (0xf5): CF was 1 → CF=0. eflags: 0x03 → 0x02.
        FlagsMiscGolden {
            name: "cmc (f5, cf=1->0)",
            code: &[0xf5],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // CLC (0xf8): CF=0. eflags: 0x03 → 0x02.
        FlagsMiscGolden {
            name: "clc (f8)",
            code: &[0xf8],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // STC (0xf9): CF=1. eflags stays 0x03 (already set).
        FlagsMiscGolden {
            name: "stc (f9)",
            code: &[0xf9],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // CLD (0xfc): DF=0. DF was already 0 in seed; eflags stays 0x03.
        FlagsMiscGolden {
            name: "cld (fc)",
            code: &[0xfc],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x3,
            eip: 0x1,
            fetch: 2,
        },
        // STD (0xfd): DF=1. eflags: 0x03 → 0x403.
        FlagsMiscGolden {
            name: "std (fd)",
            code: &[0xfd],
            gpr: [55042, 772, 1286, 16, 0, 16, 8, 24],
            eflags: 0x403,
            eip: 0x1,
            fetch: 2,
        },
    ]
}

#[test]
fn flags_misc_split_matches_golden_across_ops() {
    // The flags + misc register block (TEST r/m,reg, INC/DEC reg, CBW/CWD, SAHF/LAHF, and the
    // single flag-bit ops) is converted to the decode/execute split, so its fused arms are
    // deleted and it can no longer be diffed against a fused executor in-tree. Run each case
    // through cycle() (the split) and assert the architectural end-state against goldens
    // captured from the pre-split fused path via `regen_flags_misc_goldens`. eflags is
    // load-bearing for most cases (flags change); eip proves decode consumed the right bytes
    // (1 for implicit-operand ops, 2 for TEST with ModRM); fetch proves each instruction byte
    // was charged exactly once. INC/DEC CF-preservation is observable because the seed pre-sets
    // CF and the goldens carry it.
    for g in flags_misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        flags_misc_seed_mem(&mut mem, g.code);

        let mut split = Cpu386::default();
        flags_misc_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `flags_misc_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the fused arms (0x40-0x4f, 0x84/0x85, 0x98/0x99, 0x9e/0x9f, 0xf5/0xf8-0xfd)
/// still exist in `dispatch_opcode` (i.e. the parent commit 3912fbc5):
///   git worktree add ../regen-a7 3912fbc5
///   cd ../regen-a7
///   cargo test -p izarravm-cpu --lib regen_flags_misc_goldens -- --ignored --nocapture
/// then paste the output over `flags_misc_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_flags_misc_goldens() {
    for g in flags_misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        flags_misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        flags_misc_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            FlagsMiscGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            fetch,
        );
        if !deltas.is_empty() {
            println!("            // memory deltas: {:?}", deltas);
        }
    }
}

/// STI's interrupt shadow: after STI, the immediately-following instruction executes before
/// any interrupt is taken, even when a hardware interrupt is already pending. Drive three
/// back-to-back cycles through the split: STI then NOP (0x90) then another NOP. A fake
/// interrupt is pending from the start via `TestBus.pending_irq`. Prove: (1) after STI the
/// interrupt is NOT taken (shadow active), (2) after NOP the interrupt is still pending (shadow
/// let NOP through), and (3) after the next cycle the interrupt is consumed (shadow expired).
#[test]
fn sti_interrupt_shadow_defers_interrupt_by_one_instruction() {
    let mut memory = vec![0u8; 0x400];
    // STI (0xfb) followed by two NOPs (0x90).
    memory[0] = 0xfb; // STI
    memory[1] = 0x90; // NOP — executes before interrupt is taken (shadow)
    memory[2] = 0x90; // NOP — not reached; interrupt taken instead
    // IVT entry for vector 0x08 (IRQ0) at byte offset 0x20 (0x0008 * 4):
    // offset=0x0200, segment=0x0000.
    memory[0x20..0x22].copy_from_slice(&0x0200u16.to_le_bytes());
    memory[0x22..0x24].copy_from_slice(&0x0000u16.to_le_bytes());
    // IRET at the handler target (not reached in this test but avoids unmapped-memory errors
    // if the CPU tries to read into it).
    memory[0x200] = 0xcf;

    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x100);
    // Start with IF clear so STI is what enables interrupts.
    cpu.set_flag(FLAG_IF, false);

    let mut bus = TestBus::with_memory(memory);
    // Arm a pending IRQ 8. `interrupt_pending()` returns true while `pending_irq.is_some()`.
    bus.pending_irq = Some(8);

    // Cycle 1: STI (0xfb). IF becomes set; interrupt_shadow is armed. The pending IRQ is NOT
    // serviced yet (shadow active): eip advances to 1.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 1,
        "eip must be 1 after STI — NOP not yet executed"
    );
    assert!(cpu.flag(FLAG_IF), "STI must set IF");
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must not be taken during the STI cycle itself"
    );

    // Cycle 2: NOP (0x90). Shadow consumed at cycle start → interrupt check skipped → NOP
    // executes → eip advances to 2. IRQ still pending.
    cpu.cycle(&mut bus).unwrap();
    assert_eq!(
        cpu.registers.eip, 2,
        "eip must be 2 after NOP — shadow let NOP through"
    );
    assert!(
        bus.pending_irq.is_some(),
        "interrupt must still be pending after NOP (shadow consumed, interrupt check skipped)"
    );

    // Cycle 3: no shadow, IF set, IRQ pending → interrupt is acknowledged before fetch.
    // `acknowledge_interrupt` takes the pending_irq, so it becomes None.
    cpu.cycle(&mut bus).unwrap();
    assert!(
        bus.pending_irq.is_none(),
        "interrupt must be taken after the shadow expires"
    );
}

/// One golden end-state for a string-operation case (task A8). The string ops touch both
/// registers and memory, and the inputs differ widely per form (SI/DI/CX/AX/DF, the REP prefix,
/// the source/dest memory image), so each case carries its own register seed (`regs`) and memory
/// image (`setup`) on top of the shared `string_seed`. The captured fields are the standard
/// differential set plus the destination memory writes: end gpr (AX,CX,DX,BX,SP,BP,SI,DI),
/// eflags (CMPS/SCAS set them; MOVS/STOS/LODS leave them), eip, the (offset,value) memory deltas
/// (MOVS/STOS write the destination; CMPS/SCAS/LODS write nothing), and the InstructionPrefetch
/// fetch count (prefix + opcode, charged once in `decode` — small and CX-independent even for the
/// REP forms, since the per-element data accesses are bus reads/writes, not instruction fetches).
struct StringGolden {
    name: &'static str,
    code: &'static [u8],
    /// Per-case register seed applied after `string_seed` (SI/DI/CX/AX, the DF flag, segment
    /// bases for the override case), applied identically on the split and fused-reference paths.
    regs: fn(&mut Cpu386),
    /// Per-case memory image (the source and destination bytes), applied identically on both
    /// paths before the run.
    setup: fn(&mut [u8]),
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Shared register seed for the string-operation golden battery: CS/DS/ES/SS = 0 and eip = 0.
/// Everything that varies per form (the index registers, the count, the accumulator, DF, and the
/// ES base for the segment-override case) is set by each case's `regs` closure, so the seed itself
/// stays minimal and every input is explicit at the case site.
fn string_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
}

/// The string-operation differential battery (task A8). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_string_goldens`; see `flags_misc_golden_cases` for
/// the capture recipe. Never edit by hand — re-run the regen WHILE the fused arms (0xa4-0xa7,
/// 0xaa-0xaf) still exist in `dispatch_opcode`, then paste, then delete the fused arms.
///
/// Covers the plain single-step forms (MOVSB forward DF=0 and backward DF=1; MOVSW; CMPSB
/// flags+advance; STOSB; LODSB; SCASB; the DS:SI segment override) AND the REP forms, which are
/// the load-bearing cases: REP MOVSB (CX iterations → CX=0, every element copied, SI/DI advanced
/// by CX*width), REPE CMPSB (early termination on the first mismatch → CX and ZF prove where it
/// stopped), and REPNE SCASB (early termination on the first match → CX and ZF).
fn string_golden_cases() -> &'static [StringGolden] {
    &[
        // MOVSB forward (0xa4), DF=0: [ds:si]=0x42 at 0x100 → [es:di] at 0x200; SI/DI increment.
        StringGolden {
            name: "movsb df=0 (a4)",
            code: &[0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100] = 0x42,
            gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x42)],
            fetch: 2,
        },
        // MOVSB backward (0xa4), DF=1: same copy, but SI/DI decrement.
        StringGolden {
            name: "movsb df=1 (a4)",
            code: &[0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, true);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100] = 0x42,
            gpr: [0, 0, 0, 0, 0, 0, 0x0ff, 0x1ff],
            eflags: 0x402,
            eip: 0x1,
            deltas: &[(0x200, 0x42)],
            fetch: 2,
        },
        // MOVSW (0xa5), DF=0: word [0x100..0x102]=0x1234 → [0x200..0x202]; SI/DI += 2.
        StringGolden {
            name: "movsw df=0 (a5)",
            code: &[0xa5],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x100..0x102].copy_from_slice(&0x1234u16.to_le_bytes()),
            gpr: [0, 0, 0, 0, 0, 0, 0x102, 0x202],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x34), (0x201, 0x12)],
            fetch: 2,
        },
        // CMPSB unequal (0xa6): [ds:si]=0x10, [es:di]=0x20 → 0x10-0x20 borrows (ZF=0, CF=1);
        // SI/DI advance even on mismatch. No memory write.
        StringGolden {
            name: "cmpsb unequal (a6)",
            code: &[0xa6],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
            },
            setup: |m| {
                m[0x100] = 0x10;
                m[0x200] = 0x20;
            },
            gpr: [0, 0, 0, 0, 0, 0, 0x101, 0x201],
            eflags: 0x87,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // STOSB (0xaa): AL=0x5a → [es:di]=0x200; DI increments. AL preserved.
        StringGolden {
            name: "stosb (aa)",
            code: &[0xaa],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, 0x5a);
                c.registers.set_edi(0x200);
            },
            setup: |_m| {},
            gpr: [0x5a, 0, 0, 0, 0, 0, 0, 0x201],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[(0x200, 0x5a)],
            fetch: 2,
        },
        // LODSB (0xac): [ds:si]=0x7e at 0x100 → AL; SI increments. No memory write.
        StringGolden {
            name: "lodsb (ac)",
            code: &[0xac],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
            },
            setup: |m| m[0x100] = 0x7e,
            gpr: [0x7e, 0, 0, 0, 0, 0, 0x101, 0],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // SCASB equal (0xae): AL=0x41, [es:di]=0x41 → ZF set; DI increments, SI untouched.
        StringGolden {
            name: "scasb equal (ae)",
            code: &[0xae],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, 0x41);
                c.registers.set_edi(0x200);
            },
            setup: |m| m[0x200] = 0x41,
            gpr: [0x41, 0, 0, 0, 0, 0, 0, 0x201],
            eflags: 0x46,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
        },
        // MOVSB with an ES: source segment override (0x26 0xa4): ds=0, es base 0x200, so the source
        // reads from es:si (0x210), not ds:si (0x10); the destination stays es:di (0x230).
        StringGolden {
            name: "es: movsb override (26 a4)",
            code: &[0x26, 0xa4],
            regs: |c| {
                c.load_segment_real(SegmentIndex::Es, 0x20); // base 0x200
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x10);
                c.registers.set_edi(0x30);
            },
            setup: |m| m[0x210] = 0x99,
            gpr: [0, 0, 0, 0, 0, 0, 0x11, 0x31],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(0x230, 0x99)],
            fetch: 3,
        },
        // REP MOVSB (0xf3 0xa4), CX=3, DF=0: copies 3 bytes [0x100..0x103]→[0x200..0x203];
        // CX→0, SI/DI advance by 3. The fetch count is small (prefix+opcode), CX-independent.
        StringGolden {
            name: "rep movsb cx=3 (f3 a4)",
            code: &[0xf3, 0xa4],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
                c.registers.set_ecx(3);
            },
            setup: |m| m[0x100..0x103].copy_from_slice(&[1, 2, 3]),
            gpr: [0, 0, 0, 0, 0, 0, 0x103, 0x203],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[(0x200, 1), (0x201, 2), (0x202, 3)],
            fetch: 3,
        },
        // REPE CMPSB (0xf3 0xa6), CX=4, DF=0: "AABB" vs "AACC" mismatches at index 2, so the
        // repeat stops there with ZF clear after 3 iterations; CX 4→3→2→1, SI/DI advance by 3.
        StringGolden {
            name: "repe cmpsb cx=4 (f3 a6)",
            code: &[0xf3, 0xa6],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.registers.set_esi(0x100);
                c.registers.set_edi(0x200);
                c.registers.set_ecx(4);
            },
            setup: |m| {
                m[0x100..0x104].copy_from_slice(b"AABB");
                m[0x200..0x204].copy_from_slice(b"AACC");
            },
            gpr: [0, 1, 0, 0, 0, 0, 0x103, 0x203],
            eflags: 0x97,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
        // REPNE SCASB (0xf2 0xae), CX=4, AL='C', DF=0: dest "AACA" scans until the match at
        // index 2, stopping with ZF set after 3 iterations; CX 4→3→2→1, DI advances by 3.
        StringGolden {
            name: "repne scasb cx=4 (f2 ae)",
            code: &[0xf2, 0xae],
            regs: |c| {
                c.set_flag(FLAG_DF, false);
                c.write_gpr8(0, b'C');
                c.registers.set_edi(0x200);
                c.registers.set_ecx(4);
            },
            setup: |m| m[0x200..0x204].copy_from_slice(b"AACA"),
            gpr: [0x43, 1, 0, 0, 0, 0, 0, 0x203],
            eflags: 0x46,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
        },
    ]
}

#[test]
fn string_split_matches_golden_across_ops() {
    // The string-operation block (MOVS/CMPS/STOS/LODS/SCAS and the REP/REPE/REPNE forms) is
    // converted to the decode/execute split, so its fused arms are deleted and it can no longer be
    // diffed against a fused executor in-tree. Run each case through cycle() (the split) and
    // assert the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_string_goldens`. The register file proves SI/DI/CX/AX moved correctly (direction,
    // element width, REP count decremented to 0 or stopped early); eflags is load-bearing for
    // CMPS/SCAS; the memory deltas prove the destination image (MOVS/STOS) is byte-exact; and the
    // fetch count proves each instruction-fetch byte (prefix + opcode) was charged exactly once
    // regardless of how many elements the REP loop processed.
    for g in string_golden_cases() {
        let mut mem = vec![0u8; 0x400];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);

        let mut split = Cpu386::default();
        string_seed(&mut split);
        (g.regs)(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        for &(offset, value) in g.deltas {
            assert_eq!(
                sbus.memory[offset], value,
                "memory[{offset:#x}] mismatch for {}",
                g.name
            );
        }
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `string_golden_cases` from the fused reference. Ignored by default. Run WHILE the
/// fused arms (0xa4-0xa7, 0xaa-0xaf) still exist in `dispatch_opcode` (i.e. the parent commit
/// a9e0fec0):
///   git worktree add ../regen-a8 a9e0fec0
///   cd ../regen-a8
///   cargo test -p izarravm-cpu --lib regen_string_goldens -- --ignored --nocapture
/// then paste the output over `string_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_string_goldens() {
    for g in string_golden_cases() {
        let mut mem = vec![0u8; 0x400];
        mem[..g.code.len()].copy_from_slice(g.code);
        (g.setup)(&mut mem);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        string_seed(&mut fused);
        (g.regs)(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            // {}: gpr {:?}, eflags {:#x}, eip {:#x}, deltas {:?}, fetch {}",
            g.name,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A9: port I/O golden battery ──────────────────────────────────────────────────────────

/// One golden end-state for a port-I/O case (task A9). Port reads via TestBus always return 0,
/// so the captured GPR array reflects the read-zero / write-no-register-change behaviour. The
/// eflags field is always 0x2 (IN/OUT do not modify flags). `eip` proves decode consumed the
/// right number of bytes (2 for imm8 forms, 1 for DX forms). `fetch` proves each instruction
/// byte was charged exactly once (3 for imm8 forms = 1 prefetch-peek + 1 opcode + 1 imm,
/// 2 for DX forms = 1 prefetch-peek + 1 opcode).
struct PortIoGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    fetch: usize,
}

/// The port-I/O differential battery (task A9). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_port_io_goldens`; see `flags_misc_golden_cases`
/// for the full capture recipe. Never edit by hand — re-run the regen WHILE the fused arms
/// (0xe4-0xe7, 0xec-0xef) still exist in `dispatch_opcode` (i.e. parent commit 21cc68ba),
/// then paste, then delete the fused arms.
///
/// Seed: seam_seed — EAX=0x0102 (AL=0x02, AH=0x01), CX=0x0304, DX=0x0506, BX=0x0010,
/// SP=0, BP=0x0010, SI=0x0008, DI=0x0018, eflags=0x2. TestBus.read_io always returns 0.
/// Covers: IN AL imm8, IN AX imm8 (byte vs word width); OUT imm8 AL, OUT imm8 AX (no-op on
/// registers); IN AL DX, IN AX DX (port from DX=0x0506); OUT DX AL, OUT DX AX.
fn port_io_golden_cases() -> &'static [PortIoGolden] {
    &[
        // IN AL, imm8 (0xe4 0x78): port 0x78 → AL=0. AH unchanged → AX=0x0100, eip=2, fetch=3.
        PortIoGolden {
            name: "in al,imm8 (e4 78)",
            code: &[0xe4, 0x78],
            gpr: [0x0100, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // IN AX, imm8 (0xe5 0x78): port 0x78 → AX=0x0000 (word read), eip=2, fetch=3.
        PortIoGolden {
            name: "in ax,imm8 (e5 78)",
            code: &[0xe5, 0x78],
            gpr: [0x0000, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // OUT imm8, AL (0xe6 0x78): writes AL=0x02 to port 0x78, no register change. eip=2, fetch=3.
        PortIoGolden {
            name: "out imm8,al (e6 78)",
            code: &[0xe6, 0x78],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // OUT imm8, AX (0xe7 0x78): writes AX=0x0102 to port 0x78, no register change. eip=2, fetch=3.
        PortIoGolden {
            name: "out imm8,ax (e7 78)",
            code: &[0xe7, 0x78],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x2,
            fetch: 3,
        },
        // IN AL, DX (0xec): port=DX=0x0506 → AL=0. AH unchanged → AX=0x0100, eip=1, fetch=2.
        PortIoGolden {
            name: "in al,dx (ec)",
            code: &[0xec],
            gpr: [0x0100, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // IN AX, DX (0xed): port=DX=0x0506 → AX=0x0000 (word), eip=1, fetch=2.
        PortIoGolden {
            name: "in ax,dx (ed)",
            code: &[0xed],
            gpr: [0x0000, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // OUT DX, AL (0xee): writes AL=0x02 to port DX=0x0506, no register change. eip=1, fetch=2.
        PortIoGolden {
            name: "out dx,al (ee)",
            code: &[0xee],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
        // OUT DX, AX (0xef): writes AX=0x0102 to port DX=0x0506, no register change. eip=1, fetch=2.
        PortIoGolden {
            name: "out dx,ax (ef)",
            code: &[0xef],
            gpr: [0x0102, 0x0304, 0x0506, 0x0010, 0, 0x0010, 0x0008, 0x0018],
            eflags: 0x2,
            eip: 0x1,
            fetch: 2,
        },
    ]
}

#[test]
fn port_io_split_matches_golden_across_ops() {
    // The port I/O block (IN/OUT byte-imm-port and DX-port forms) is converted to the
    // decode/execute split, so its fused arms are deleted and it can no longer be diffed
    // against a fused executor in-tree. Run each case through cycle() (the split) and assert
    // the architectural end-state against goldens captured from the pre-split fused path via
    // `regen_port_io_goldens`. eip proves decode consumed the right number of bytes (2 for
    // imm8 forms, 1 for DX forms); fetch proves each instruction byte was charged exactly
    // once. TestBus.read_io returns 0, so IN forms zero the accumulator (AL or AX); OUT
    // forms leave registers unchanged.
    for g in port_io_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        mem[..g.code.len()].copy_from_slice(g.code);

        let mut split = Cpu386::default();
        seam_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `port_io_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the fused arms (0xe4-0xe7, 0xec-0xef) still exist in `dispatch_opcode`
/// (i.e. the parent commit 21cc68ba):
///   git worktree add ../regen-a9 21cc68ba
///   cd ../regen-a9
///   cargo test -p izarravm-cpu --lib regen_port_io_goldens -- --ignored --nocapture
/// then paste the output over `port_io_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_port_io_goldens() {
    for g in port_io_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        mem[..g.code.len()].copy_from_slice(g.code);

        let mut fused = Cpu386::default();
        seam_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            PortIoGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            fetch,
        );
    }
}

// ── Task A10: bit-manipulation golden battery ─────────────────────────────────────────────────

/// One golden end-state for a bit-manipulation case (task A10). BT/BTS/BTR/BTC, BSF/BSR,
/// SHLD/SHRD, CMPXCHG, and XADD all set flags (CF for BT-family, ZF for BSF/BSR/CMPXCHG, the
/// full ALU set for SHLD/SHRD/CMPXCHG/XADD), write registers, and — for the memory r/m forms —
/// write memory, so this captures the full register file, eflags, eip, memory-write deltas, and
/// the InstructionPrefetch fetch count. `eip` proves decode consumed the right number of bytes
/// (incl. the 0F second byte and the imm8 for 0F BA/A4/AC); `fetch` proves each instruction byte
/// was charged exactly once.
struct BitManipGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the bit-manipulation golden battery. Real-mode, DS=0, with a scratch word region
/// the memory r/m forms address. Registers are chosen so each op has a non-trivial, observable
/// result: BX=3 (a bit index that exercises CF and the set/reset/toggle write-backs), CX=0x0008
/// (so the BTR/BTC register cases find bit 3 already set), and a known pattern at the scratch
/// region for the memory BT-walk cases. The instruction is placed at offset 0; the scratch
/// region starts at 0x40.
fn bitmanip_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    // AX=0x0034 (accumulator for CMPXCHG: matches the planted dest 0x0034 so the equal branch
    // fires), CX=0x0008 (bit 3 set, for BTR/BTC register cases), DX=0x0506, BX=3 (bit index /
    // CMPXCHG-XADD source), SP=0x00f0, BP=0x0010, SI=0x0008, DI=0x0018.
    cpu.write_reg16(Reg16::Ax, 0x0034);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Dx, 0x0506);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    // eflags: only the always-set reserved bit 1.
    cpu.registers.eflags = 0x02;
}

/// Lay the instruction bytes at offset 0 and plant the scratch data the memory r/m forms read.
/// Word at 0x40 = 0x1234 (the BTS positive-index walk lands in the NEXT word at 0x42, proving
/// the bit-offset addressing), byte at 0x40 = 0x34 also serves as the CMPXCHG/XADD byte dest.
fn bitmanip_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // Scratch words: 0x40 = 0x1234, 0x42 = 0x0000, 0x44 = 0xffff (so a positive walk into 0x42
    // sets a bit in a zero word, observable as a clean single-byte delta).
    mem[0x40..0x42].copy_from_slice(&0x1234u16.to_le_bytes());
    mem[0x42..0x44].copy_from_slice(&0x0000u16.to_le_bytes());
    mem[0x44..0x46].copy_from_slice(&0xffffu16.to_le_bytes());
}

/// The bit-manipulation differential battery (task A10). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_bitmanip_goldens`; see `alu_golden_cases` for the
/// full capture recipe. Never edit by hand — re-run the regen from the pre-split commit
/// (parent 430a6051) WHILE the fused arms (0F A3/AB/B3/BB/BA/BC/BD/A4/A5/AC/AD/B0/B1/C0/C1)
/// still exist in `execute_two_byte`, then paste, then delete the fused arms.
fn bitmanip_golden_cases() -> &'static [BitManipGolden] {
    &[
        // BT CX, BX (0F A3 D9): test bit BX=3 of CX=0x0008 (bit 3 set) -> CF=1, no write.
        BitManipGolden {
            name: "bt cx,bx (0f a3 d9)",
            code: &[15, 163, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTS CX, BX (0F AB D9): set bit 3 of CX=0x0008 (already set) -> CF=1, CX unchanged.
        BitManipGolden {
            name: "bts cx,bx (0f ab d9)",
            code: &[15, 171, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTR CX, BX (0F B3 D9): reset bit 3 of CX=0x0008 -> CF=1 (old bit), CX=0x0000.
        BitManipGolden {
            name: "btr cx,bx (0f b3 d9)",
            code: &[15, 179, 217],
            gpr: [52, 0, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTC CX, BX (0F BB D9): toggle bit 3 of CX=0x0008 -> CF=1 (old), CX=0x0000.
        BitManipGolden {
            name: "btc cx,bx (0f bb d9)",
            code: &[15, 187, 217],
            gpr: [52, 0, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BTS [0x40], BX (0F AB 1E 40 00): BX=3 -> sets bit 3 of the word at 0x40=0x1234.
        // (No walk: index 3 < 16, lands in the first word.) 0x1234 has bit 3 clear, so the low
        // byte goes 0x34 -> 0x3c (=60): delta (64, 60). CF=0 (old bit clear).
        BitManipGolden {
            name: "bts [0x40],bx no-walk (0f ab 1e 40 00)",
            code: &[15, 171, 30, 64, 0],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(64, 60)],
            fetch: 6,
        },
        // BTS [0x40], DX with DX=16 -> bit index 16 walks to the NEXT word at 0x42 (the subtle
        // BT-memory case): sets bit 0 of the 0x0000 word at 0x42, so the delta is at byte 66
        // (=0x42), NOT the base 0x40. This is the load-bearing assertion for bit-offset
        // addressing: the write must land in the adjacent element. DX is overridden to 16.
        BitManipGolden {
            name: "bts [0x40],dx walk-to-next-word (0f ab 16 40 00)",
            code: &[15, 171, 22, 64, 0],
            gpr: [52, 8, 16, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(66, 1)],
            fetch: 6,
        },
        // BTS [0x40], imm8=5 (0F BA 2E 40 00 05): /5=BTS, fixed imm8 index 5 -> bit 5 of the
        // word at 0x40=0x1234 is already set, so CF=1 and NO memory write (no delta). Proves the
        // imm8 form addresses the base word and the unchanged-write path.
        BitManipGolden {
            name: "bts [0x40],5 (0f ba 2e 40 00 05)",
            code: &[15, 186, 46, 64, 0, 5],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x6,
            deltas: &[],
            fetch: 7,
        },
        // BT CX, imm8=3 (0F BA E1 03): /4=BT, mod=3 rm=CX -> CF = bit 3 of CX=0x0008 = 1.
        BitManipGolden {
            name: "bt cx,3 (0f ba e1 03)",
            code: &[15, 186, 225, 3],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x3,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // BSF BX, CX (0F BC D9): CX=0x0008 -> lowest set bit index 3 into BX, ZF=0.
        BitManipGolden {
            name: "bsf bx,cx (0f bc d9)",
            code: &[15, 188, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BSR BX, CX (0F BD D9): CX=0x0008 -> highest set bit index 3 into BX, ZF=0.
        BitManipGolden {
            name: "bsr bx,cx (0f bd d9)",
            code: &[15, 189, 217],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // BSF BX, CX with CX=0 (0F BC D9, CX overridden to 0): ZF=1 (eflags 0x42), BX preserved
        // at its preset 0xbeef (=48879). Proves the zero-source path leaves the destination.
        BitManipGolden {
            name: "bsf bx,cx zero-src (0f bc d9)",
            code: &[15, 188, 217],
            gpr: [52, 0, 1286, 48879, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SHLD AX, BX, imm8=4 (0F A4 D8 04): mod=3 reg=BX rm=AX. AX=0x0034, BX=3 -> shifts AX
        // left 4, filling from BX's high bits -> AX=0x0340 (=832). Proves the imm8 count + flags.
        BitManipGolden {
            name: "shld ax,bx,4 (0f a4 d8 04)",
            code: &[15, 164, 216, 4],
            gpr: [832, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // SHRD AX, BX, imm8=4 (0F AC D8 04): shifts AX right 4, filling from BX's low bits ->
        // AX=0x3003 (=12291), CF=1 + PF (eflags 0x6).
        BitManipGolden {
            name: "shrd ax,bx,4 (0f ac d8 04)",
            code: &[15, 172, 216, 4],
            gpr: [12291, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
        },
        // SHLD AX, BX, CL (0F A5 D8): CL=8 (CX=0x0008 -> CL=8) -> shift AX left 8 -> AX=0x3400
        // (=13312).
        BitManipGolden {
            name: "shld ax,bx,cl (0f a5 d8)",
            code: &[15, 165, 216],
            gpr: [13312, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SHRD AX, BX, CL (0F AD D8): CL=8 -> shift AX right 8 -> AX=0x0300 (=768).
        BitManipGolden {
            name: "shrd ax,bx,cl (0f ad d8)",
            code: &[15, 173, 216],
            gpr: [768, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMPXCHG [0x40], BL byte form (0F B0 1E 40 00): AL=0x34 == dest byte 0x34 -> equal:
        // ZF=1 (eflags 0x46), store BL=3 into [0x40]: delta (64, 3). The equal branch + write.
        BitManipGolden {
            name: "cmpxchg [0x40],bl equal (0f b0 1e 40 00)",
            code: &[15, 176, 30, 64, 0],
            gpr: [52, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x46,
            eip: 0x5,
            deltas: &[(64, 3)],
            fetch: 6,
        },
        // CMPXCHG CX, BX word form (0F B1 D9): AX=0x0034 != CX=0x0008 -> unequal: ZF=0
        // (eflags 0x12), load CX into AX (AX=0x0008). Register dest, the unequal re-write.
        BitManipGolden {
            name: "cmpxchg cx,bx unequal (0f b1 d9)",
            code: &[15, 177, 217],
            gpr: [8, 8, 1286, 3, 240, 16, 8, 24],
            eflags: 0x12,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // XADD BL, CL byte form (0F C0 CB): mod=3 reg=CL(1) rm=BL(3). dest=BL=3, src=CL=8 ->
        // BL=11, CL=3 (old dest), flags like ADD(3,8).
        BitManipGolden {
            name: "xadd bl,cl (0f c0 cb)",
            code: &[15, 192, 203],
            gpr: [52, 3, 1286, 11, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // XADD [0x40], CX word form (0F C1 0E 40 00): dest=word[0x40]=0x1234, src=CX=0x0008 ->
        // [0x40]=0x123c (low byte 0x34 -> 0x3c=60: delta (64, 60)), CX=0x1234 (=4660, old dest),
        // flags like ADD. Proves the memory XADD path.
        BitManipGolden {
            name: "xadd [0x40],cx (0f c1 0e 40 00)",
            code: &[15, 193, 14, 64, 0],
            gpr: [52, 4660, 1286, 3, 240, 16, 8, 24],
            eflags: 0x6,
            eip: 0x5,
            deltas: &[(64, 60)],
            fetch: 6,
        },
    ]
}

/// Per-case register overrides applied AFTER `bitmanip_seed`, so a few cases can drive an
/// operand the default seed doesn't cover (the BT-memory walk needs DX=16; the BSF zero-source
/// case needs CX=0). Applied identically on both the split and the fused (regen) path so the
/// goldens stay a faithful differential. Returns None when the default seed suffices.
fn bitmanip_case_override(name: &str, cpu: &mut Cpu386) {
    match name {
        "bts [0x40],dx walk-to-next-word (0f ab 16 40 00)" => {
            // DX=16 so the bit index walks one 16-bit element past 0x40, into the word at 0x42.
            cpu.write_reg16(Reg16::Dx, 16);
        }
        "bsf bx,cx zero-src (0f bc d9)" => {
            cpu.write_reg16(Reg16::Cx, 0);
            cpu.write_reg16(Reg16::Bx, 0xbeef); // preset so "destination unchanged" is visible
        }
        _ => {}
    }
}

#[test]
fn bitmanip_split_matches_golden_across_ops() {
    // The bit-manipulation opcodes (BT/BTS/BTR/BTC reg+imm8, BSF/BSR, SHLD/SHRD imm8+CL,
    // CMPXCHG, XADD) are converted to the decode/execute split, so their fused arms are deleted
    // and they can no longer be diffed against a fused executor in-tree. Run each case through
    // cycle() (the split) and assert the architectural end-state against goldens captured from
    // the pre-split fused path via `regen_bitmanip_goldens`. The register file proves the
    // set/reset/toggle write-backs, BSF/BSR indices, double-shift results, and the CMPXCHG/XADD
    // exchanges; eflags proves CF (BT-family), ZF (BSF/BSR/CMPXCHG), and the ALU flags
    // (SHLD/SHRD/CMPXCHG/XADD); the memory deltas prove the memory r/m write path — crucially
    // the BT-memory walk lands the write in the ADJACENT word, not the base word; eip + fetch
    // prove decode consumed and charged every byte (0F prefix + ModRM + imm8) exactly once.
    for g in bitmanip_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        bitmanip_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        bitmanip_seed(&mut split);
        bitmanip_case_override(g.name, &mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `bitmanip_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the bit-manipulation fused arms still exist in `execute_two_byte`
/// (i.e. the parent commit 430a6051):
///   git worktree add ../regen-a10 430a6051
///   cd ../regen-a10
///   cargo test -p izarravm-cpu --lib regen_bitmanip_goldens -- --ignored --nocapture
/// then paste the output over `bitmanip_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_bitmanip_goldens() {
    for g in bitmanip_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        bitmanip_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        bitmanip_seed(&mut fused);
        bitmanip_case_override(g.name, &mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            BitManipGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A11: condmove golden battery ────────────────────────────────────────────────────────

/// One golden end-state for a condmove case (task A11). CMOVcc, SETcc, and IMUL reg,r/m all
/// touch the register file and/or memory and leave eflags unchanged (CMOVcc/SETcc) or set
/// CF/OF (IMUL), so this captures the full register file, eflags, eip, memory-write deltas,
/// and the InstructionPrefetch fetch count. `eip` proves decode consumed the right number of
/// bytes (incl. the 0F second byte and the ModRM+displacement); `fetch` proves each byte
/// was charged exactly once.
struct CondMoveGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
}

/// Seed for the condmove golden battery. Real-mode, DS=0, 16-bit addressing. AX=5, BX=3,
/// CX=0x0100, DX=0x4000; eflags has ZF=0 (only the reserved bit-1). Scratch memory at
/// 0x40 holds the word 0x0003 (CMOVcc memory source); byte at 0x50 is zero (SETcc mem dest).
fn condmove_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 5);
    cpu.write_reg16(Reg16::Bx, 3);
    cpu.write_reg16(Reg16::Cx, 0x0100);
    cpu.write_reg16(Reg16::Dx, 0x4000);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02; // ZF=0
}

fn condmove_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x40..0x42].copy_from_slice(&3u16.to_le_bytes()); // word 3 for CMOVcc memory source
}

/// The condmove differential battery (task A11). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy`) via `regen_condmove_goldens` (parent commit 93bdff3f) WHILE
/// the fused arms (CMOVcc 0x40-0x4F, SETcc 0x90-0x9F, IMUL 0xAF) still existed in
/// `execute_two_byte`. Never edit by hand — re-run the regen from the pre-split commit.
fn condmove_golden_cases() -> &'static [CondMoveGolden] {
    &[
        // SETcc false: SETZ AL (0F 94 C0): ZF=0 → condition false → AL=0 (AX=0x0000).
        CondMoveGolden {
            name: "setz al false (0f 94 c0)",
            code: &[15, 148, 192],
            gpr: [0, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SETcc true: SETNZ BL (0F 95 C3): ZF=0 → condition true → BL=1 (BX=0x0001).
        CondMoveGolden {
            name: "setnz bl true (0f 95 c3)",
            code: &[15, 149, 195],
            gpr: [5, 256, 16384, 1, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // SETcc mem false: SETZ [0x50] (0F 94 1E 50 00): ZF=0 → write 0 to [0x50] (no delta, mem
        // already 0). Proves the byte-wide memory write fires even for the false condition.
        CondMoveGolden {
            name: "setz [0x50] false (0f 94 1e 50 00)",
            code: &[15, 148, 30, 80, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // SETcc mem true: SETNZ [0x50] (0F 95 1E 50 00): ZF=0 → write 1 to [0x50]; delta (80, 1).
        CondMoveGolden {
            name: "setnz [0x50] true (0f 95 1e 50 00)",
            code: &[15, 149, 30, 80, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(80, 1)],
            fetch: 6,
        },
        // CMOVcc false: CMOVZ AX, BX (0F 44 C3): ZF=0 → condition false → AX unchanged (=5).
        CondMoveGolden {
            name: "cmovz ax,bx false (0f 44 c3)",
            code: &[15, 68, 195],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMOVcc true: CMOVNZ AX, BX (0F 45 C3): ZF=0 → condition true → AX = BX = 3.
        CondMoveGolden {
            name: "cmovnz ax,bx true (0f 45 c3)",
            code: &[15, 69, 195],
            gpr: [3, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // CMOVcc mem false: CMOVZ AX, [0x40] (0F 44 06 40 00): ZF=0 → AX unchanged; the
        // memory source is still read (architectural: memory operand is always fetched).
        CondMoveGolden {
            name: "cmovz ax,[0x40] false (0f 44 06 40 00)",
            code: &[15, 68, 6, 64, 0],
            gpr: [5, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // CMOVcc mem true: CMOVNZ AX, [0x40] (0F 45 06 40 00): ZF=0 → AX = [0x40] = 3.
        CondMoveGolden {
            name: "cmovnz ax,[0x40] true (0f 45 06 40 00)",
            code: &[15, 69, 6, 64, 0],
            gpr: [3, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
        },
        // IMUL no overflow: IMUL AX, BX (0F AF C3): 5*3=15, fits in 16 bits → CF=OF=0.
        CondMoveGolden {
            name: "imul ax,bx no-overflow (0f af c3)",
            code: &[15, 175, 195],
            gpr: [15, 256, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
        // IMUL overflow: IMUL CX, DX (0F AF CA): 0x0100*0x4000=0x400000, truncated to
        // CX=0x0000 → CF=OF=1 (eflags 0x803: bit11=OF, bit1=reserved, bit0=CF).
        CondMoveGolden {
            name: "imul cx,dx overflow (0f af ca)",
            code: &[15, 175, 202],
            gpr: [5, 0, 16384, 3, 240, 16, 8, 24],
            eflags: 0x803,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
        },
    ]
}

#[test]
fn condmove_split_matches_golden_across_ops() {
    // The condmove opcodes (CMOVcc, SETcc, IMUL reg,r/m) are converted to the decode/execute
    // split, so their fused arms are deleted and they can no longer be diffed against a fused
    // executor in-tree. Run each case through cycle() (the split) and assert the architectural
    // end-state against goldens captured from the pre-split fused path (parent 93bdff3f) via
    // `regen_condmove_goldens`. The register file proves SETcc byte writes (true/false both
    // register and memory), CMOVcc destination changed-or-unchanged, and IMUL product;
    // eflags proves SETcc/CMOVcc leave flags unchanged and IMUL sets CF/OF on overflow;
    // the memory deltas prove SETcc writes a 0 or 1 correctly; eip + fetch prove decode
    // consumed and charged every byte (0F prefix + ModRM + displacement) exactly once.
    for g in condmove_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        condmove_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        condmove_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        let _ = split.cycle(&mut sbus);

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `condmove_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the condmove fused arms still exist in `execute_two_byte`
/// (i.e. the parent commit 93bdff3f):
///   git worktree add ../regen-a11 93bdff3f
///   cd ../regen-a11
///   cargo test -p izarravm-cpu --lib regen_condmove_goldens -- --ignored --nocapture
/// then paste the output over `condmove_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_condmove_goldens() {
    for g in condmove_golden_cases() {
        let mut mem = vec![0u8; 0x100];
        condmove_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        condmove_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            CondMoveGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
        );
    }
}

// ── Task A12: system / descriptor-table / segment-load golden battery ──────────────────────────

/// One golden end-state for a system / descriptor-table / segment-load case (task A12). These
/// opcodes change a heterogeneous set of architectural state — GPRs (SLDT/STR/SMSW/LAR/LSL store
/// a selector/limit; LES/LDS load the offset), eflags (VERR/VERW/LAR/LSL set ZF), memory
/// (SGDT/SIDT store the pseudo-descriptor, SMSW r/m16 stores to memory), the descriptor tables
/// (LGDT/LIDT), the control registers (MOV CR, LMSW, CLTS), the LDTR/TR selectors (LLDT/LTR), and
/// the ES/DS segment registers (LES/LDS) — so the golden captures all of them. `eip` proves
/// decode consumed the right byte count (incl. the 0F second byte + ModRM + displacement);
/// `fetch` proves each instruction byte was charged exactly once.
struct SystemSegGolden {
    name: &'static str,
    code: &'static [u8],
    /// Whether the case runs in protected mode (CR0.PE set, the seeded GDT live).
    protected: bool,
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
    cr0: u32,
    gdtr_base: u32,
    gdtr_limit: u16,
    idtr_base: u32,
    idtr_limit: u16,
    ldtr_sel: u16,
    tr_sel: u16,
    es_sel: u16,
    ds_sel: u16,
}

/// Seed for the system/segment golden battery. Real or protected mode (CR0.PE per the case),
/// 16-bit addressing, DS=0. A GDT lives at base 0x100, limit 0xff, with descriptors planted at
/// selectors 0x08 (a present readable data segment, access 0x92, byte-granular limit 0xffff),
/// 0x10 (a present available 386 TSS, access 0x89), and 0x18 (a present LDT system descriptor,
/// access 0x82). CR0 carries TS|MP (0x0A) plus PE when protected. gdtr/idtr/ldtr/tr start at
/// known values so the load ops (LGDT/LIDT/LLDT/LTR) and the store ops (SGDT/SIDT/SLDT/STR/SMSW)
/// both have an observable before/after. Registers: CX=0x0008 (a selector operand for LAR/LSL/
/// LLDT/LTR/VERR/VERW), the rest a fixed pattern.
fn system_seg_seed(cpu: &mut Cpu386, protected: bool) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Es, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x0005);
    cpu.write_reg16(Reg16::Cx, 0x0008);
    cpu.write_reg16(Reg16::Dx, 0x4000);
    cpu.write_reg16(Reg16::Bx, 0x0003);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02;
    cpu.gdtr = DescriptorTable {
        base: 0x100,
        limit: 0xff,
    };
    cpu.idtr = DescriptorTable {
        base: 0x900,
        limit: 0x3ff,
    };
    cpu.ldtr.selector = 0x0028;
    cpu.tr.selector = 0x0038;
    cpu.control.cr0 = CR0_TS | CR0_MP;
    if protected {
        cpu.control.cr0 |= CR0_PE;
    }
}

/// Plant the instruction bytes plus the GDT descriptors and the scratch the memory forms read.
fn system_seg_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    // GDT at 0x100. Selector 0x08: present readable data segment (access 0x92), limit 0xffff.
    mem[0x108..0x10c].copy_from_slice(&0x0000_ffffu32.to_le_bytes());
    mem[0x10c..0x110].copy_from_slice(&0x0000_9200u32.to_le_bytes());
    // Selector 0x10: present available 386 TSS (access 0x89), base 0x0005_0000, limit 0x0067.
    mem[0x110..0x114].copy_from_slice(&0x0000_0067u32.to_le_bytes());
    mem[0x114..0x118].copy_from_slice(&0x0005_8900u32.to_le_bytes());
    // Selector 0x18: present LDT system descriptor (access 0x82), base 0x0006_0000, limit 0x0fff.
    mem[0x118..0x11c].copy_from_slice(&0x0000_0fffu32.to_le_bytes());
    mem[0x11c..0x120].copy_from_slice(&0x0006_8200u32.to_le_bytes());
    // A 6-byte GDTR/IDTR pseudo-descriptor image at 0x40 (limit 0x00ff, base 0x0000_1000) for
    // LGDT/LIDT, and bounds [10, 20] at 0x80/0x84 for BOUND, and a far pointer 0x09:0x1234 at
    // 0x90 for LES/LDS.
    mem[0x40..0x46].copy_from_slice(&[0xff, 0x00, 0x00, 0x10, 0x00, 0x00]);
    mem[0x80..0x82].copy_from_slice(&10u16.to_le_bytes());
    mem[0x82..0x84].copy_from_slice(&20u16.to_le_bytes());
    mem[0x90..0x92].copy_from_slice(&0x1234u16.to_le_bytes()); // offset
    mem[0x92..0x94].copy_from_slice(&0x0009u16.to_le_bytes()); // selector (RPL 1 -> sel 0x08)
}

/// The system/segment differential battery (task A12). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy` -> `execute_two_byte`/`dispatch_opcode`) via
/// `regen_system_seg_goldens` (parent commit b0a4262d) WHILE the fused arms (0F 00/01/02/03/06/
/// 20/22, BOUND 0x62, LES/LDS 0xc4/0xc5) still existed. Never edit by hand — re-run the regen
/// from the pre-split commit.
fn system_seg_golden_cases() -> &'static [SystemSegGolden] {
    // Captured verbatim from the fused reference at parent b0a4262d via
    // `regen_system_seg_goldens` (run in a throwaway worktree). Never edit by hand.
    &[
        SystemSegGolden {
            name: "smsw ax (0f 01 e0)",
            code: &[15, 1, 224],
            protected: false,
            gpr: [10, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "smsw [0x60] (0f 01 26 60 00)",
            code: &[15, 1, 38, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 10)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lmsw ax (0f 01 f0)",
            code: &[15, 1, 240],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0x5,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "clts (0f 06)",
            code: &[15, 6],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            cr0: 0x2,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "sgdt [0x60] (0f 01 06 60 00)",
            code: &[15, 1, 6, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 255), (99, 1)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "sidt [0x60] (0f 01 0e 60 00)",
            code: &[15, 1, 14, 96, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[(96, 255), (97, 3), (99, 9)],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lgdt [0x40] (0f 01 16 40 00)",
            code: &[15, 1, 22, 64, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x1000,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lidt [0x40] (0f 01 1e 40 00)",
            code: &[15, 1, 30, 64, 0],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x5,
            deltas: &[],
            fetch: 6,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x1000,
            idtr_limit: 0xff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "mov eax,cr0 (0f 20 c0)",
            code: &[15, 32, 192],
            protected: false,
            gpr: [10, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "mov cr2,eax (0f 22 d0)",
            code: &[15, 34, 208],
            protected: false,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "bound ax,[0x80] in-range (62 06 80 00)",
            code: &[98, 6, 128, 0],
            protected: false,
            gpr: [15, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "les bx,[0x90] (c4 1e 90 00)",
            code: &[196, 30, 144, 0],
            protected: false,
            gpr: [5, 8, 16384, 4660, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x9,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lds bx,[0x90] (c5 1e 90 00)",
            code: &[197, 30, 144, 0],
            protected: false,
            gpr: [5, 8, 16384, 4660, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            cr0: 0xa,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x9,
        },
        SystemSegGolden {
            name: "sldt ax (0f 00 c0)",
            code: &[15, 0, 192],
            protected: true,
            gpr: [40, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "str ax (0f 00 c8)",
            code: &[15, 0, 200],
            protected: true,
            gpr: [56, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lldt cx=0x18 (0f 00 d1)",
            code: &[15, 0, 209],
            protected: true,
            gpr: [5, 24, 16384, 3, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x18,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "verr cx (0f 00 e1)",
            code: &[15, 0, 225],
            protected: true,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "verw cx (0f 00 e9)",
            code: &[15, 0, 233],
            protected: true,
            gpr: [5, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lar ax,cx (0f 02 c1)",
            code: &[15, 2, 193],
            protected: true,
            gpr: [37376, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
        SystemSegGolden {
            name: "lsl ax,cx (0f 03 c1)",
            code: &[15, 3, 193],
            protected: true,
            gpr: [65535, 8, 16384, 3, 240, 16, 8, 24],
            eflags: 0x42,
            eip: 0x3,
            deltas: &[],
            fetch: 4,
            cr0: 0xb,
            gdtr_base: 0x100,
            gdtr_limit: 0xff,
            idtr_base: 0x900,
            idtr_limit: 0x3ff,
            ldtr_sel: 0x28,
            tr_sel: 0x38,
            es_sel: 0x0,
            ds_sel: 0x0,
        },
    ]
}

/// Per-case register overrides applied AFTER `system_seg_seed`. LLDT needs CX pointing at the
/// LDT system descriptor (selector 0x18); BOUND and LES/LDS need their default seed. Applied
/// identically on the split and the regen (fused) path so the goldens stay a faithful diff.
fn system_seg_case_override(name: &str, cpu: &mut Cpu386) {
    if name == "lldt cx=0x18 (0f 00 d1)" {
        cpu.write_reg16(Reg16::Cx, 0x18);
    }
    if name == "bound ax,[0x80] in-range (62 06 80 00)" {
        cpu.write_reg16(Reg16::Ax, 15);
    }
}

fn assert_system_seg_state(cpu: &Cpu386, g: &SystemSegGolden) {
    assert_eq!(cpu.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
    assert_eq!(cpu.eflags(), g.eflags, "eflags mismatch for {}", g.name);
    assert_eq!(cpu.registers.eip, g.eip, "eip mismatch for {}", g.name);
    assert_eq!(cpu.control.cr0, g.cr0, "cr0 mismatch for {}", g.name);
    assert_eq!(
        cpu.gdtr.base, g.gdtr_base,
        "gdtr.base mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.gdtr.limit, g.gdtr_limit,
        "gdtr.limit mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.idtr.base, g.idtr_base,
        "idtr.base mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.idtr.limit, g.idtr_limit,
        "idtr.limit mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.ldtr.selector, g.ldtr_sel,
        "ldtr selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.tr.selector, g.tr_sel,
        "tr selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Es).selector,
        g.es_sel,
        "es selector mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ds).selector,
        g.ds_sel,
        "ds selector mismatch for {}",
        g.name
    );
}

#[test]
fn system_seg_split_matches_golden_across_ops() {
    // The system / descriptor-table / segment-load opcodes (0F 00/01/02/03/06/20/22, BOUND,
    // LES/LDS) are converted to the decode/execute split, so their fused arms are deleted and
    // they can no longer be diffed against a fused executor in-tree. Run each case through the
    // split (`exec_one_split`) and assert the architectural end-state — GPRs, eflags, the
    // control register, the GDTR/IDTR, the LDTR/TR selectors, and the ES/DS segment selectors —
    // against goldens captured from the pre-split fused path (parent b0a4262d) via
    // `regen_system_seg_goldens`. eip + fetch prove decode consumed and charged every byte (0F
    // prefix + ModRM + displacement) exactly once; the memory deltas prove the SGDT/SIDT/SMSW
    // store path; the CR/descriptor/segment fields prove the load ops drove the right state
    // through the reused leaf helpers (so the TLB/code-cache invalidation hooks still fire).
    for g in system_seg_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        system_seg_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        system_seg_seed(&mut split, g.protected);
        system_seg_case_override(g.name, &mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_system_seg_state(&split, g);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// Regenerate `system_seg_golden_cases` from the fused reference. Ignored by default.
/// Run WHILE the system/segment fused arms still exist (parent commit b0a4262d):
///   git worktree add ../regen-a12 b0a4262d
///   cd ../regen-a12
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_system_seg_goldens -- --ignored --nocapture
/// then paste the output over `system_seg_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_system_seg_goldens() {
    for g in system_seg_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        system_seg_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        system_seg_seed(&mut fused, g.protected);
        system_seg_case_override(g.name, &mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        println!(
            "            SystemSegGolden {{ name: {:?}, code: &{:?}, protected: {}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {}, cr0: {:#x}, gdtr_base: {:#x}, gdtr_limit: {:#x}, idtr_base: {:#x}, idtr_limit: {:#x}, ldtr_sel: {:#x}, tr_sel: {:#x}, es_sel: {:#x}, ds_sel: {:#x} }},",
            g.name,
            g.code,
            g.protected,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
            fused.control.cr0,
            fused.gdtr.base,
            fused.gdtr.limit,
            fused.idtr.base,
            fused.idtr.limit,
            fused.ldtr.selector,
            fused.tr.selector,
            fused.registers.segment(SegmentIndex::Es).selector,
            fused.registers.segment(SegmentIndex::Ds).selector,
        );
    }
}

// ---- Task A13: x87 FPU (0xD8-0xDF) + WAIT (0x9B) decode/execute split ----

struct FpuGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
    fpu_control: u16,
    fpu_status: u16,
    fpu_tag: u16,
    /// The architectural stack ST(0)..ST(7), each f64 captured as raw bits (NaN-stable).
    st: [u64; 8],
}

/// Seed for the x87 FPU golden battery. Real mode, CS=DS=SS=0, 16-bit addressing. The FPU is
/// reset (FINIT state) and then ST(1)=1.25, ST(0)=3.5 are pushed so the stack ops (FADD ST0,ST1;
/// FXCH; FST; FNSTSW; FCOM; ...) have stable, distinct inputs; TOP therefore starts at 6. A
/// non-default control word (0x027f, the FINIT default) and a status condition are left as the
/// push set them. GPRs are a fixed pattern (AX..DI) so the FNSTSW-AX / integer-flag forms have an
/// observable before/after.
fn fpu_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Ax, 0x1111);
    cpu.write_reg16(Reg16::Cx, 0x2222);
    cpu.write_reg16(Reg16::Dx, 0x3333);
    cpu.write_reg16(Reg16::Bx, 0x4444);
    cpu.write_reg16(Reg16::Sp, 0x00f0);
    cpu.write_reg16(Reg16::Bp, 0x0010);
    cpu.write_reg16(Reg16::Si, 0x0008);
    cpu.write_reg16(Reg16::Di, 0x0018);
    cpu.registers.eflags = 0x02;
    cpu.fpu.finit();
    cpu.fpu.push(1.25); // ST(1)
    cpu.fpu.push(3.5); // ST(0)
}

/// Plant the instruction bytes plus the float/int scratch the memory forms read. A 4-byte real
/// 2.0 at [0x100], an 8-byte real 1.5 at [0x108], a 4-byte int 7 at [0x110], a 2-byte int 9 at
/// [0x118], and a 16-bit control word 0x037f at [0x120] (for FLDCW). The store forms write into
/// the free area at [0x130] onward.
fn fpu_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x100..0x104].copy_from_slice(&2.0f32.to_le_bytes());
    mem[0x108..0x110].copy_from_slice(&1.5f64.to_le_bytes());
    mem[0x110..0x114].copy_from_slice(&7i32.to_le_bytes());
    mem[0x118..0x11a].copy_from_slice(&9i16.to_le_bytes());
    mem[0x120..0x122].copy_from_slice(&0x037fu16.to_le_bytes());
}

fn assert_fpu_state(cpu: &Cpu386, g: &FpuGolden) {
    assert_eq!(cpu.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
    assert_eq!(cpu.eflags(), g.eflags, "eflags mismatch for {}", g.name);
    assert_eq!(cpu.registers.eip, g.eip, "eip mismatch for {}", g.name);
    assert_eq!(
        cpu.fpu.control, g.fpu_control,
        "fpu control mismatch for {}",
        g.name
    );
    assert_eq!(
        cpu.fpu.status, g.fpu_status,
        "fpu status mismatch for {}",
        g.name
    );
    assert_eq!(cpu.fpu.tag, g.fpu_tag, "fpu tag mismatch for {}", g.name);
    let st: [u64; 8] = std::array::from_fn(|i| cpu.fpu.get(i as u8).to_bits());
    assert_eq!(st, g.st, "fpu stack ST(0)..ST(7) mismatch for {}", g.name);
}

/// The x87 FPU differential battery (task A13). Captured from the PRIOR fused reference
/// (`execute_instruction_legacy` -> `dispatch_opcode` -> `execute_fpu`) via `regen_fpu_goldens`
/// (parent commit 0b928034) WHILE the fused 0xD8-0xDF / 0x9B arms still existed. Never edit by
/// hand — re-run the regen from the pre-split commit. Covers a representative set: a memory load
/// (FLD m32), a memory store (FST m32), an FPU stack op (FADD ST0,ST1 and FXCH), the control word
/// (FLDCW / FNSTCW), the status word (FNSTSW AX and FNSTSW m16), a few arithmetic / compare ops,
/// an integer-operand memory form (FIADD m32), and WAIT/FWAIT (0x9B).
fn fpu_golden_cases() -> &'static [FpuGolden] {
    // Captured verbatim from the fused reference at parent 0b928034 via `regen_fpu_goldens`
    // (run in a throwaway worktree). Never edit by hand.
    &[
        FpuGolden {
            name: "fwait (9b)",
            code: &[155],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x1,
            deltas: &[],
            fetch: 2,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fld m32 [0x100] (d9 06 00 01)",
            code: &[217, 6, 0, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x2800,
            fpu_tag: 0x3ff,
            st: [
                0x4000000000000000,
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fst m32 [0x130] (d9 16 30 01)",
            code: &[217, 22, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(306, 96), (307, 64)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fstp m32 [0x130] (d9 1e 30 01)",
            code: &[217, 30, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(306, 96), (307, 64)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3800,
            fpu_tag: 0x3fff,
            st: [
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x400c000000000000,
            ],
        },
        FpuGolden {
            name: "fadd st0,st1 (d8 c1)",
            code: &[216, 193],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4013000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fmul st0,st1 (d8 c9)",
            code: &[216, 201],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4011800000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fcom st1 (d8 d1)",
            code: &[216, 209],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fxch st1 (d9 c9)",
            code: &[217, 201],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x3ff4000000000000,
                0x400c000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fadd m64 [0x108] (dc 06 08 01)",
            code: &[220, 6, 8, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4014000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fiadd m32 [0x110] (da 06 10 01)",
            code: &[218, 6, 16, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x4025000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fldcw [0x120] (d9 2e 20 01)",
            code: &[217, 46, 32, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstcw [0x130] (d9 3e 30 01)",
            code: &[217, 62, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(304, 127), (305, 3)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstsw ax (df e0)",
            code: &[223, 224],
            gpr: [12288, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x2,
            deltas: &[],
            fetch: 3,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
        FpuGolden {
            name: "fnstsw m16 [0x130] (dd 3e 30 01)",
            code: &[221, 62, 48, 1],
            gpr: [4369, 8738, 13107, 17476, 240, 16, 8, 24],
            eflags: 0x2,
            eip: 0x4,
            deltas: &[(305, 48)],
            fetch: 5,
            fpu_control: 0x37f,
            fpu_status: 0x3000,
            fpu_tag: 0xfff,
            st: [
                0x400c000000000000,
                0x3ff4000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
                0x0000000000000000,
            ],
        },
    ]
}

#[test]
fn fpu_split_matches_golden_across_ops() {
    // The x87 FPU opcodes (0xD8-0xDF) and WAIT (0x9B) are converted to the decode/execute split,
    // so their fused arms are deleted and they can no longer be diffed against a fused executor
    // in-tree. Run each case through the split (`exec_one_split`) and assert the architectural
    // end-state — GPRs, eflags, the FPU control/status/tag words, and the architectural stack
    // ST(0)..ST(7) — against goldens captured from the pre-split fused path (parent 0b928034)
    // via `regen_fpu_goldens`. eip + fetch prove decode consumed and charged every byte (opcode +
    // ModRM + displacement) exactly once; the memory deltas prove the store path.
    for g in fpu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        fpu_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        fpu_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_fpu_state(&split, g);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

#[test]
fn fist_honors_rounding_control() {
    // FISTP m32 [0x130] (DB 1E 30 01) under all four RC modes. DJGPP-compiled code
    // (Quake) flips RC to chop around every C (int) cast; 80387 PRM table 15-2.
    let cases: &[(u16, f64, u32)] = &[
        (0b00, 2.5, 2), // nearest-even
        (0b01, 2.5, 2), // toward -inf
        (0b10, 2.5, 3), // toward +inf
        (0b11, 2.5, 2), // chop
        (0b00, -1.5, -2i32 as u32),
        (0b01, -1.5, -2i32 as u32),
        (0b10, -1.5, -1i32 as u32),
        (0b11, -1.5, -1i32 as u32),
    ];
    for &(rc, input, expected) in cases {
        let mut mem = vec![0u8; 0x200];
        mem[..4].copy_from_slice(&[0xdb, 0x1e, 0x30, 0x01]);
        let mut cpu = Cpu386::default();
        fpu_seed(&mut cpu);
        cpu.fpu.control = 0x037f | (rc << 10);
        cpu.fpu.push(input);
        let mut bus = TestBus::with_memory(mem);
        exec_one_split(&mut cpu, &mut bus).unwrap();
        let got = u32::from_le_bytes(bus.memory[0x130..0x134].try_into().unwrap());
        assert_eq!(got, expected, "FISTP m32 of {input} with RC={rc:02b}");
        assert_eq!(
            cpu.fpu.status & 0x01,
            0,
            "no IE for the in-range FISTP of {input}"
        );
    }
}

#[test]
fn fist_overflow_stores_integer_indefinite_and_raises_ie() {
    // Out-of-range (and NaN) FIST stores the integer indefinite for the width and
    // raises IE (masked #IA response), rather than Rust's saturating cast.
    let m16: &[u8] = &[0xdf, 0x1e, 0x30, 0x01]; // FISTP m16
    let m32: &[u8] = &[0xdb, 0x1e, 0x30, 0x01]; // FISTP m32
    let m64: &[u8] = &[0xdf, 0x3e, 0x30, 0x01]; // FISTP m64
    let cases: &[(&[u8], f64, Vec<u8>)] = &[
        (m16, 40000.0, 0x8000u16.to_le_bytes().to_vec()),
        (m16, -40000.0, 0x8000u16.to_le_bytes().to_vec()),
        (m32, 3.0e9, 0x8000_0000u32.to_le_bytes().to_vec()),
        (m64, 1.0e19, 0x8000_0000_0000_0000u64.to_le_bytes().to_vec()),
        (m32, f64::NAN, 0x8000_0000u32.to_le_bytes().to_vec()),
    ];
    for (code, input, expected) in cases {
        let mut mem = vec![0u8; 0x200];
        mem[..code.len()].copy_from_slice(code);
        let mut cpu = Cpu386::default();
        fpu_seed(&mut cpu);
        cpu.fpu.push(*input);
        let mut bus = TestBus::with_memory(mem);
        exec_one_split(&mut cpu, &mut bus).unwrap();
        let got = &bus.memory[0x130..0x130 + expected.len()];
        assert_eq!(got, expected, "indefinite for FISTP of {input}");
        assert_ne!(cpu.fpu.status & 0x01, 0, "IE raised for FISTP of {input}");
    }
}

#[test]
fn frndint_honors_rounding_control() {
    for (rc, expected) in [(0u16, -2.0), (1, -2.0), (2, -1.0), (3, -1.0)] {
        let mut cpu = Cpu386::default();
        fpu_seed(&mut cpu);
        cpu.fpu.control = 0x037f | (rc << 10);
        cpu.fpu.push(-1.5);
        let mut bus = TestBus::with_memory({
            let mut mem = vec![0u8; 0x200];
            mem[..2].copy_from_slice(&[0xd9, 0xfc]); // FRNDINT
            mem
        });
        exec_one_split(&mut cpu, &mut bus).unwrap();
        assert_eq!(cpu.fpu.get(0), expected, "FRNDINT of -1.5 with RC={rc:02b}");
    }
}

/// Run one instruction against a fresh FPU seeded with the given stack (last
/// element becomes ST(0)) and return the CPU for state assertions. The x87
/// value-accuracy battery below uses manual-cited inputs per family; the
/// differential goldens above pin encodings, these pin VALUES.
fn fpu_exec(code: &[u8], stack: &[f64]) -> (Cpu386, TestBus) {
    let mut mem = vec![0u8; 0x200];
    mem[..code.len()].copy_from_slice(code);
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.eflags = 0x02;
    cpu.fpu.finit();
    for &v in stack {
        cpu.fpu.push(v);
    }
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    (cpu, bus)
}

/// Condition codes C3/C2/C1/C0 from the status word, as a tuple.
fn cc(cpu: &Cpu386) -> (bool, bool, bool, bool) {
    let s = cpu.fpu.status;
    (
        s & (1 << 14) != 0,
        s & (1 << 10) != 0,
        s & (1 << 9) != 0,
        s & (1 << 8) != 0,
    )
}

#[test]
fn fld_fstp_m80_round_trips_exact_values() {
    // FLD m80 [0x100]: 1.5 in extended = sign 0, exponent 16383, mantissa
    // 0xC000000000000000 (explicit integer bit + 0.5), 80387 PRM data formats.
    let mut mem = vec![0u8; 0x200];
    mem[..4].copy_from_slice(&[0xdb, 0x2e, 0x00, 0x01]); // FLD tbyte [0x100]
    mem[0x100..0x108].copy_from_slice(&0xC000_0000_0000_0000u64.to_le_bytes());
    mem[0x108..0x10a].copy_from_slice(&0x3FFFu16.to_le_bytes());
    let mut cpu = Cpu386::default();
    fpu_seed(&mut cpu);
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1.5, "FLD m80 of extended 1.5");

    // FSTP m80 [0x130] of -2.0: sign 1, exponent 16384, integer-bit-only mantissa.
    let (_, bus) = fpu_exec(&[0xdb, 0x3e, 0x30, 0x01], &[-2.0]);
    assert_eq!(
        bus.memory[0x130..0x138],
        0x8000_0000_0000_0000u64.to_le_bytes(),
        "FSTP m80 mantissa of -2.0"
    );
    assert_eq!(
        bus.memory[0x138..0x13a],
        0xC000u16.to_le_bytes(),
        "FSTP m80 sign+exponent of -2.0"
    );
}

#[test]
fn faulting_push_leaves_sp_unchanged() {
    // A push whose stack write faults must leave (E)SP at its
    // pre-instruction value so the restart after the handler re-executes
    // cleanly (386 PRM fault-restart semantics). CWSDPMI grows the DJGPP
    // stack by committing the page in its #PF handler and retrying; a
    // committed-then-faulted ESP double-decrements on the retry.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x8000); // stack target beyond the test memory
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x50; // PUSH AX
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x8000,
        "SP unchanged after the faulting push"
    );
}

#[test]
fn fpatan_quadrants() {
    // FPATAN: ST1 = atan(ST1/ST0) with quadrant correction, then pop.
    // atan2(1, -1) = 3pi/4 (80387 PRM: operand signs select the quadrant).
    let (cpu, _) = fpu_exec(&[0xd9, 0xf3], &[1.0, -1.0]); // ST1=1 (y), ST0=-1 (x)
    let want = 3.0 * std::f64::consts::FRAC_PI_4;
    assert!(
        (cpu.fpu.get(0) - want).abs() < 1e-15,
        "FPATAN(y=1, x=-1) = 3pi/4, got {}",
        cpu.fpu.get(0)
    );
}

#[test]
fn fprem_positive_quotient_low_bits_land_in_c0_c3_c1() {
    // FPREM: 17 mod 5 = 2 with quotient 3; C0/C3/C1 = quotient bits 2/1/0 =
    // 0/1/1, C2 = 0 (reduction complete). 80387 PRM FPREM description.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf8], &[5.0, 17.0]); // ST1=5, ST0=17
    assert_eq!(cpu.fpu.get(0), 2.0, "17 rem 5");
    let (c3, c2, c1, c0) = cc(&cpu);
    assert!(!c2, "C2 clear: reduction complete");
    assert!(!c0 && c3 && c1, "quotient 3 = 0b011 in C0/C3/C1");
}

#[test]
fn fprem1_uses_round_to_nearest_quotient() {
    // FPREM1 separates from FPREM at 8 mod 5: the IEEE nearest quotient of
    // 8/5 = 1.6 is 2, remainder -2 (FPREM's truncated quotient 1 leaves +3).
    let (cpu, _) = fpu_exec(&[0xd9, 0xf5], &[5.0, 8.0]);
    assert_eq!(cpu.fpu.get(0), -2.0, "FPREM1 8 rem 5 (nearest quotient 2)");
    let (_, c2, _, _) = cc(&cpu);
    assert!(!c2);
}

#[test]
fn fxtract_splits_exponent_and_significand() {
    // FXTRACT on 6.0: exponent 2 replaces ST(0), significand 1.5 is pushed.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf4], &[6.0]);
    assert_eq!(cpu.fpu.get(0), 1.5, "significand of 6.0");
    assert_eq!(cpu.fpu.get(1), 2.0, "unbiased exponent of 6.0");
}

#[test]
fn fscale_truncates_the_scale_toward_zero() {
    // FSCALE: ST0 = ST0 * 2^trunc(ST1); the fractional and negative scales
    // truncate toward zero (the integer case is covered by
    // `fscale_scales_by_power_of_two`). trunc(2.5) = 2 -> 12; trunc(-1.5) =
    // -1 -> 1.5. 80387 PRM FSCALE.
    let (cpu, _) = fpu_exec(&[0xd9, 0xfd], &[2.5, 3.0]);
    assert_eq!(cpu.fpu.get(0), 12.0, "3.0 scaled by trunc(2.5)");
    let (cpu, _) = fpu_exec(&[0xd9, 0xfd], &[-1.5, 3.0]);
    assert_eq!(cpu.fpu.get(0), 1.5, "3.0 scaled by trunc(-1.5)");
}

#[test]
fn fxam_classifies_and_signs() {
    // FXAM: C3/C2/C0 classify ST(0), C1 = sign. 80387 PRM table: zero = C3,
    // NaN = C0, infinity = C2+C0, normal = C2, empty = C3+C0.
    let cases: &[(f64, (bool, bool, bool))] = &[
        (0.0, (true, false, false)),
        (f64::NAN, (false, false, true)),
        (f64::INFINITY, (false, true, true)),
        (1.0, (false, true, false)),
    ];
    for &(v, (want_c3, want_c2, want_c0)) in cases {
        let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[v]);
        let (c3, c2, _, c0) = cc(&cpu);
        assert_eq!((c3, c2, c0), (want_c3, want_c2, want_c0), "FXAM of {v}");
    }
    let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[-1.0]);
    let (_, _, c1, _) = cc(&cpu);
    assert!(c1, "FXAM C1 = sign of -1.0");
    let (cpu, _) = fpu_exec(&[0xd9, 0xe5], &[]);
    let (c3, _, _, c0) = cc(&cpu);
    assert!(c3 && c0, "FXAM of an empty ST(0)");
}

#[test]
fn f2xm1_and_fyl2x_hit_exact_and_near_values() {
    // F2XM1 on 0.5: 2^0.5 - 1 = sqrt(2) - 1.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf0], &[0.5]);
    assert!(
        (cpu.fpu.get(0) - (std::f64::consts::SQRT_2 - 1.0)).abs() < 1e-15,
        "F2XM1(0.5)"
    );
    // FYL2X: ST1 * log2(ST0), pop. 3 * log2(8) = 9 exactly in f64.
    let (cpu, _) = fpu_exec(&[0xd9, 0xf1], &[3.0, 8.0]); // ST1=3, ST0=8
    assert_eq!(cpu.fpu.get(0), 9.0, "FYL2X exact case");
    assert_eq!(cpu.fpu.top(), 7, "FYL2X popped once from a 2-deep stack");
}

#[test]
fn fsincos_pushes_cos_over_sin() {
    // FSINCOS on 0.0: ST(1) = sin = 0, ST(0) = cos = 1, C2 = 0.
    let (cpu, _) = fpu_exec(&[0xd9, 0xfb], &[0.0]);
    assert_eq!(cpu.fpu.get(0), 1.0, "cos(0)");
    assert_eq!(cpu.fpu.get(1), 0.0, "sin(0)");
    let (_, c2, _, _) = cc(&cpu);
    assert!(!c2, "C2 clear: argument in range");
}

#[test]
fn fcompp_compares_and_pops_both() {
    // FCOMPP (DE D9): compare ST(0) with ST(1), pop both. 2 < 3 -> C0 set.
    let (cpu, _) = fpu_exec(&[0xde, 0xd9], &[3.0, 2.0]); // ST1=3, ST0=2
    let (c3, _, _, c0) = cc(&cpu);
    assert!(c0 && !c3, "2 < 3 sets C0");
    assert_eq!(cpu.fpu.top(), 0, "both operands popped");
    assert!(cpu.fpu.is_empty(0), "stack empty after FCOMPP");
}

#[test]
fn fbld_fbstp_round_trip_packed_bcd() {
    // FBLD [0x100] of packed BCD 1234567; FBSTP writes the digits back with
    // the sign in bit 7 of byte 9. 80387 PRM packed-BCD format.
    let mut mem = vec![0u8; 0x200];
    mem[..4].copy_from_slice(&[0xdf, 0x26, 0x00, 0x01]); // FBLD [0x100]
    mem[0x100] = 0x67;
    mem[0x101] = 0x45;
    mem[0x102] = 0x23;
    mem[0x103] = 0x01;
    let mut cpu = Cpu386::default();
    fpu_seed(&mut cpu);
    let mut bus = TestBus::with_memory(mem);
    exec_one_split(&mut cpu, &mut bus).unwrap();
    assert_eq!(cpu.fpu.get(0), 1234567.0, "FBLD 1234567");

    let (cpu, bus) = fpu_exec(&[0xdf, 0x36, 0x30, 0x01], &[-1234567.0]); // FBSTP [0x130]
    assert_eq!(
        &bus.memory[0x130..0x134],
        &[0x67, 0x45, 0x23, 0x01],
        "FBSTP digits"
    );
    assert_eq!(bus.memory[0x139], 0x80, "FBSTP sign byte for a negative");
    assert_eq!(cpu.fpu.top(), 0, "FBSTP popped");
}

#[test]
fn faulting_push_leaves_esp_unchanged_on_a_32bit_stack() {
    // The SS.B=1 arm - the one a DPMI flat 32-bit stack (CWSDPMI/DJGPP)
    // actually exercises: the full ESP must stay at its pre-instruction
    // value when the push's write faults.
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.registers.set_segment(
        SegmentIndex::Ss,
        SegmentRegister {
            selector: 0x10,
            base: 0,
            limit: 0xffff_ffff,
            access: 0x93,
            default_size_32: true,
        },
    );
    cpu.registers.eip = 0;
    cpu.registers.set_esp(0x0001_8000); // beyond the 0x200-byte test memory
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x50; // PUSH (E)AX
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.registers.esp(),
        0x0001_8000,
        "ESP unchanged after the faulting push on a 32-bit stack"
    );
}

#[test]
fn faulting_pusha_restores_sp_past_committed_pushes() {
    // PUSHA: the first two pushes land, the third faults; (E)SP must come
    // back to the pre-instruction value (386 PRM: PUSHA restores ESP so
    // the whole instruction restarts).
    let mut cpu = Cpu386::default();
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.write_reg16(Reg16::Sp, 0x0004); // AX@2, CX@0 land; DX@0xfffe faults
    let mut mem = vec![0u8; 0x200];
    mem[0] = 0x60; // PUSHA
    let mut bus = TestBus::with_memory(mem);
    assert!(exec_one_split(&mut cpu, &mut bus).is_err());
    assert_eq!(
        cpu.read_reg16(Reg16::Sp),
        0x0004,
        "SP restored after the faulting PUSHA"
    );
}

/// Regenerate `fpu_golden_cases` from the fused reference. Ignored by default. Run WHILE the
/// x87 fused arms still exist (parent commit 0b928034):
///   git worktree add ../regen-a13 0b928034
///   cd ../regen-a13
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_fpu_goldens -- --ignored --nocapture
/// then paste the output over `fpu_golden_cases` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_fpu_goldens() {
    for g in fpu_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        fpu_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        fpu_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        let st: [u64; 8] = std::array::from_fn(|i| fused.fpu.get(i as u8).to_bits());
        println!(
            "            FpuGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {}, fpu_control: {:#x}, fpu_status: {:#x}, fpu_tag: {:#x}, st: [{} ] }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
            fused.fpu.control,
            fused.fpu.status,
            fused.fpu.tag,
            st.iter()
                .map(|b| format!(" {b:#018x},"))
                .collect::<String>(),
        );
    }
}

// ── Task A14: the heterogeneous one-off golden battery ─────────────────────────────────────────

/// One golden end-state for a Misc case (task A14). Captures the architectural register file
/// (AX,CX,DX,BX,SP,BP,SI,DI), eflags, eip, the (offset,value) memory writes, the instruction-
/// fetch cycle count, and the MMX register file + x87 tag word (so the MMX/EMMS members are
/// covered too). Port reads via TestBus always return 0, so the IN/OUT-derived register/memory
/// values reflect the read-zero behaviour; the port traffic itself is asserted separately by the
/// dedicated INS/OUTS tests.
struct MiscGolden {
    name: &'static str,
    code: &'static [u8],
    gpr: [u32; 8],
    eflags: u32,
    eip: u32,
    deltas: &'static [(usize, u8)],
    fetch: usize,
    mmx: [u64; 8],
    fpu_tag: u16,
}

/// Seed for the Misc golden battery: a fixed register file giving BCD/IMUL/TEST/XLAT/MMX stable
/// inputs. AL=0x29, AH=0x05 (so DAA/AAA/AAM/AAD/TEST exercise the adjust/flag paths); CF/AF preset
/// so DAA/DAS see an incoming carry; BX=0x10 (XLAT base); CX/DX/SI/DI/BP fixed. EDX:EAX and ECX:EBX
/// are also given known 32-bit halves for CMPXCHG8B (set after this via the high words below).
fn misc_seed(cpu: &mut Cpu386) {
    cpu.load_segment_real(SegmentIndex::Cs, 0);
    cpu.load_segment_real(SegmentIndex::Ds, 0);
    cpu.load_segment_real(SegmentIndex::Ss, 0);
    cpu.registers.eip = 0;
    cpu.registers.set_eax(0x0000_0529); // AL=0x29, AH=0x05
    cpu.registers.set_ecx(0x0000_0304);
    cpu.registers.set_edx(0x0000_0506);
    cpu.registers.set_ebx(0x0000_0010);
    cpu.registers.set_esi(0x0000_0008);
    cpu.registers.set_edi(0x0000_0018);
    cpu.registers.set_ebp(0x0000_0010);
    cpu.registers.eflags = 0x13; // CF=1, AF=1 (bit 4) on top of the always-1 bit 1
    // Seed the MMX register file so MOVQ/Pxxx/EMMS have non-trivial inputs.
    cpu.fpu.set_mm(0, 0x0102_0304_0506_0708);
    cpu.fpu.set_mm(1, 0x1010_1010_1010_1010);
}

/// Seed memory for the Misc battery: plant the XLAT lookup table byte at [BX+AL]=[0x39], an
/// m64 for CMPXCHG8B at [0x40], and a packed-byte source for the MMX memory form at [0x100].
fn misc_seed_mem(mem: &mut [u8], code: &[u8]) {
    mem[..code.len()].copy_from_slice(code);
    mem[0x39] = 0xab; // XLAT: [DS:BX+AL] with BX=0x10, AL=0x29 -> 0x39
    mem[0x40..0x48].copy_from_slice(&0x0102_0304_0506_0708u64.to_le_bytes()); // CMPXCHG8B m64
    mem[0x100..0x108].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]); // MMX m64 source
}

/// The heterogeneous one-off differential battery (task A14). Captured from the PRIOR fused
/// reference (`execute_instruction_legacy`) via `regen_misc_goldens` at parent commit f1d65e0f
/// WHILE the fused arms (single-byte 0x27/0x2f/0x37/0x3f/0x69/0x6b/0x6c-0x6f/0xa8/0xa9/0xd4/0xd5/
/// 0xd6/0xd7/0xf4 and the 0F CMPXCHG8B/MMX/CPUID/RDTSC/...) still existed. Never edit by hand —
/// re-run the regen from the pre-split commit. Covers: DAA/DAS/AAA/AAS (BCD flag effects),
/// AAM/AAD (incl. the imm8 base), TEST AL/AX,imm (flags only), IMUL r,r/m,imm8/imm16 (OF/CF set),
/// SALC, XLAT (memory read), HLT, CPUID, RDTSC, MOVD/MOVQ/PADDB/EMMS (MMX), and CMPXCHG8B.
fn misc_golden_cases() -> &'static [MiscGolden] {
    // Captured verbatim from the fused reference at parent f1d65e0f via `regen_misc_goldens`
    // (run in a throwaway worktree). Never edit by hand.
    MISC_GOLDEN_CASES
}

/// The captured Misc golden literals. The `code`/`name` are authored; the remaining fields are
/// the fused reference's end-state, pasted verbatim from `regen_misc_goldens` (parent f1d65e0f).
/// gpr/code are the regen's printed (decimal) literals; do not hand-edit.
const MISC_GOLDEN_CASES: &[MiscGolden] = &[
    MiscGolden {
        name: "daa (27)",
        code: &[39],
        gpr: [1423, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x93,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "das (2f)",
        code: &[47],
        gpr: [1475, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x97,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "aaa (37)",
        code: &[55],
        gpr: [1551, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "aas (3f)",
        code: &[63],
        gpr: [1027, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "aam (d4 0a)",
        code: &[212, 10],
        gpr: [1025, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "aad (d5 0a)",
        code: &[213, 10],
        gpr: [91, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "test al,imm8 (a8 0f)",
        code: &[168, 15],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x16,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "test ax,imm16 (a9 ff 00)",
        code: &[169, 255, 0],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x12,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "imul ax,bx,imm8 (6b c3 02)",
        code: &[107, 195, 2],
        gpr: [32, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x12,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "imul ax,bx,imm16 (69 c3 00 40)",
        code: &[105, 195, 0, 64],
        gpr: [0, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x813,
        eip: 0x4,
        deltas: &[],
        fetch: 5,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "salc (d6)",
        code: &[214],
        gpr: [1535, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "xlat (d7)",
        code: &[215],
        gpr: [1451, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x1,
        deltas: &[],
        fetch: 2,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "rdtsc (0f 31)",
        code: &[15, 49],
        gpr: [0, 772, 0, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "movd mm0,eax (0f 6e c0)",
        code: &[15, 110, 192],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
        mmx: [
            0x0000000000000529,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "movq mm1,mm0 (0f 6f c8)",
        code: &[15, 111, 200],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x3,
        deltas: &[],
        fetch: 4,
        mmx: [
            0x0102030405060708,
            0x0102030405060708,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "paddb mm0,[0x100] (0f fc 06 00 01)",
        code: &[15, 252, 6, 0, 1],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x5,
        deltas: &[],
        fetch: 6,
        mmx: [
            0x0909090909090909,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
    MiscGolden {
        name: "emms (0f 77)",
        code: &[15, 119],
        gpr: [1321, 772, 1286, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x2,
        deltas: &[],
        fetch: 3,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0xffff,
    },
    MiscGolden {
        name: "cmpxchg8b [0x40] (0f c7 0e 40 00)",
        code: &[15, 199, 14, 64, 0],
        gpr: [84281096, 772, 16909060, 16, 0, 16, 8, 24],
        eflags: 0x13,
        eip: 0x5,
        deltas: &[],
        fetch: 6,
        mmx: [
            0x0102030405060708,
            0x1010101010101010,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
            0x0000000000000000,
        ],
        fpu_tag: 0x0,
    },
];

#[test]
fn misc_split_matches_golden_across_ops() {
    // The Misc one-off opcodes are converted to the decode/execute split, so their fused arms
    // are deleted and they can no longer be diffed against a fused executor in-tree. Run each
    // case through the split (`exec_one_split`) and assert the architectural end-state — GPRs,
    // eflags, the MMX file + x87 tag, and the memory writes — against goldens captured from the
    // pre-split fused path (parent f1d65e0f) via `regen_misc_goldens`. eip + fetch prove decode
    // consumed and charged every byte (opcode + ModRM + displacement + immediate) exactly once.
    for g in misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut split = Cpu386::default();
        misc_seed(&mut split);
        let mut sbus = TestBus::with_memory(mem);
        exec_one_split(&mut split, &mut sbus).unwrap();

        assert_eq!(split.registers.gpr, g.gpr, "gpr mismatch for {}", g.name);
        assert_eq!(split.eflags(), g.eflags, "eflags mismatch for {}", g.name);
        assert_eq!(split.registers.eip, g.eip, "eip mismatch for {}", g.name);
        let mmx: [u64; 8] = std::array::from_fn(|i| split.fpu.mm(i as u8));
        assert_eq!(mmx, g.mmx, "mmx register mismatch for {}", g.name);
        assert_eq!(split.fpu.tag, g.fpu_tag, "fpu tag mismatch for {}", g.name);
        let deltas: Vec<(usize, u8)> = sbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        assert_eq!(deltas, g.deltas, "memory-write mismatch for {}", g.name);
        assert_eq!(
            seam_fetch_count(&sbus),
            g.fetch,
            "instruction-fetch cycle count mismatch for {} (seam must charge fetches once)",
            g.name
        );
    }
}

/// AAM with a base of 0 is a divide error (#DE) — the only Misc op that faults on its operand,
/// so it is asserted here (through the split) rather than carried as a golden end-state.
/// (`aam_zero_divisor_is_divide_error` covers the same via `cycle`; this pins the split decode
/// path specifically: decode fetches the imm8 base, the executor raises #DE on base 0.)
#[test]
fn misc_aam_base_zero_is_divide_error() {
    let (mut cpu, memory) = real_mode_cpu(&[0xd4, 0x00], 0x20);
    let mut bus = TestBus::with_memory(memory);
    assert!(
        matches!(
            exec_one_split(&mut cpu, &mut bus),
            Err(InternalFault::Exception {
                vector: 0,
                error_code: None
            })
        ),
        "AAM base 0 must raise a deliverable #DE through the split"
    );
}

/// Regenerate `MISC_GOLDEN_CASES` from the fused reference. Ignored by default. Run WHILE the
/// fused one-off arms still exist (parent commit f1d65e0f):
///   git worktree add ../regen-a14 f1d65e0f
///   cd ../regen-a14
///   # paste this test + the cases/seed/struct in, then:
///   cargo test -p izarravm-cpu --lib regen_misc_goldens -- --ignored --nocapture
/// then paste the output over `MISC_GOLDEN_CASES` and only then delete the fused arms.
#[test]
#[ignore = "prints golden literals; run with --ignored --nocapture against the fused reference"]
fn regen_misc_goldens() {
    for g in misc_golden_cases() {
        let mut mem = vec![0u8; 0x200];
        misc_seed_mem(&mut mem, g.code);
        let initial = mem.clone();

        let mut fused = Cpu386::default();
        misc_seed(&mut fused);
        let mut fbus = TestBus::with_memory(mem);
        fused.begin_instruction();
        if exec_one_split(&mut fused, &mut fbus).is_err() {
            println!(
                "            // TODO regen {}: fused path unavailable here; run against the base commit",
                g.name
            );
            continue;
        }
        let deltas: Vec<(usize, u8)> = fbus
            .memory
            .iter()
            .enumerate()
            .filter(|(i, b)| **b != initial[*i])
            .map(|(i, b)| (i, *b))
            .collect();
        let fetch = seam_fetch_count(&fbus);
        let mmx: [u64; 8] = std::array::from_fn(|i| fused.fpu.mm(i as u8));
        println!(
            "    MiscGolden {{ name: {:?}, code: &{:?}, gpr: {:?}, eflags: {:#x}, eip: {:#x}, deltas: &{:?}, fetch: {}, mmx: [{} ], fpu_tag: {:#x} }},",
            g.name,
            g.code,
            fused.registers.gpr,
            fused.eflags(),
            fused.registers.eip,
            deltas,
            fetch,
            mmx.iter()
                .map(|b| format!(" {b:#018x},"))
                .collect::<String>(),
            fused.fpu.tag,
        );
    }
}

#[test]
fn eager_flag_write_after_pending_is_correct() {
    // A pending ADD sets CF; a later CLC-like set_flag must clear CF while leaving the
    // pending-derived ZF intact, without forcing the rest of the lazy flags live.
    let mut cpu = Cpu386::default();
    let r = cpu.alu_add_eager(0xff, 0x01, 0, BusWidth::Byte); // CF=1, ZF=1 (result 0x00)
    let lf = LazyFlags {
        a: 0xff,
        b: 0x01,
        result: r,
        width: BusWidth::Byte,
        op: LazyFlagOp::Add,
        cf_override: None,
    };
    let mut lazy = Cpu386 {
        pending_flags: PendingFlags::from_legacy(&lf),
        ..Default::default()
    };
    lazy.reset_perf_counters();
    lazy.set_flag(FLAG_CF, false); // CLC-like eager write
    assert!(!lazy.flag(FLAG_CF), "CF must be cleared by the eager write");
    assert!(
        lazy.flag(FLAG_ZF),
        "ZF from the pending descriptor must survive"
    );
    assert!(
        lazy.pending_flags.tag & (1u32 << 31) != 0,
        "single-CF writes should use the lazy CF override"
    );
    assert_eq!(
        lazy.perf.flag_materializations, 0,
        "CF override should not materialize lazy flags"
    );
}

#[test]
fn non_arithmetic_flag_write_after_pending_stays_lazy() {
    let mut lazy = Cpu386::default();
    lazy.alu_sub(1, 1, 0, BusWidth::Byte); // pending ZF=1
    lazy.reset_perf_counters();

    lazy.set_flag(FLAG_DF, true);

    assert!(lazy.flag(FLAG_DF), "DF write must be visible");
    assert!(lazy.flag(FLAG_ZF), "pending arithmetic flags must survive");
    assert!(
        lazy.pending_flags.tag & (1u32 << 31) != 0,
        "non-arithmetic writes should not settle pending arithmetic flags"
    );
    assert_eq!(
        lazy.perf.flag_materializations, 0,
        "non-arithmetic writes should not materialize lazy flags"
    );
}

#[test]
fn lazy_flag_read_matches_eager_for_add_and_sub() {
    // arith_flag computed from a pending descriptor must equal the eager eflags bit for every
    // arithmetic flag, across widths and a spread of operand pairs (incl. carry/borrow/overflow/zero).
    let cases: &[(u32, u32, BusWidth)] = &[
        (0xff, 0x01, BusWidth::Byte),
        (0x7f, 0x01, BusWidth::Byte),
        (0x00, 0x00, BusWidth::Byte),
        (0x01, 0xff, BusWidth::Byte), // a < b: SUB borrow path sets CF=1
        (0x80, 0x80, BusWidth::Byte),
        (0xffff, 0x1, BusWidth::Word),
        (0x8000, 0x8000, BusWidth::Word),
        (0xffff_ffff, 0x1, BusWidth::Dword),
        (0x1234_5678, 0x8765_4321, BusWidth::Dword),
    ];
    for &(a, b, w) in cases {
        for is_sub in [false, true] {
            let mut eager = Cpu386::default();
            let r = if is_sub {
                eager.alu_sub_eager(a, b, 0, w)
            } else {
                eager.alu_add_eager(a, b, 0, w)
            };
            let lf = LazyFlags {
                a: a & width_mask(w),
                b: b & width_mask(w),
                result: r,
                width: w,
                op: if is_sub {
                    LazyFlagOp::Sub
                } else {
                    LazyFlagOp::Add
                },
                cf_override: None,
            };
            let lazy = Cpu386 {
                pending_flags: PendingFlags::from_legacy(&lf),
                ..Default::default()
            };
            for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
                assert_eq!(
                    lazy.flag(f),
                    eager.flag(f),
                    "flag {f:#x} a={a:#x} b={b:#x} sub={is_sub} w={w:?}"
                );
            }
        }
    }
}

#[test]
fn alu_add_defers_and_reads_back_identically() {
    // alu_add (carry 0) must set a pending whose flag reads equal the eager path's eflags bit-for-bit.
    for &(a, b, w) in &[
        (0xff_u32, 0x01_u32, BusWidth::Byte),
        (0x1234_5678_u32, 0x8765_4321_u32, BusWidth::Dword),
    ] {
        let mut eager = Cpu386::default();
        let er = eager.alu_add_eager(a, b, 0, w);
        let mut lazy = Cpu386::default();
        let lr = lazy.alu_add(a, b, 0, w);
        assert_eq!(lr, er, "result");
        assert!(
            lazy.pending_flags.tag & (1u32 << 31) != 0,
            "carry-0 ADD must defer"
        );
        for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
            assert_eq!(lazy.flag(f), eager.flag(f), "flag {f:#x}");
        }
    }
}

#[test]
fn alu_sub_defers_and_reads_back_identically() {
    // alu_sub (borrow 0) must set a pending whose flag reads equal the eager path's eflags bit-for-bit.
    for &(a, b, w) in &[
        (0x01_u32, 0xff_u32, BusWidth::Byte),
        (0x1234_5678_u32, 0x8765_4321_u32, BusWidth::Dword),
    ] {
        let mut eager = Cpu386::default();
        let er = eager.alu_sub_eager(a, b, 0, w);
        let mut lazy = Cpu386::default();
        let lr = lazy.alu_sub(a, b, 0, w);
        assert_eq!(lr, er, "result");
        assert!(
            lazy.pending_flags.tag & (1u32 << 31) != 0,
            "borrow-0 SUB must defer"
        );
        for f in [FLAG_CF, FLAG_PF, FLAG_AF, FLAG_ZF, FLAG_SF, FLAG_OF] {
            assert_eq!(lazy.flag(f), eager.flag(f), "flag {f:#x}");
        }
    }
}

#[test]
fn whole_eflags_read_materializes_pending() {
    // Reading the whole eflags word (e.g. via eflags()) after a pending op must equal the eager result.
    let mut eager = Cpu386::default();
    let r = eager.alu_add_eager(0x80, 0x80, 0, BusWidth::Byte); // CF=1, OF=1, ZF=1
    let lf = LazyFlags {
        a: 0x80,
        b: 0x80,
        result: r,
        width: BusWidth::Byte,
        op: LazyFlagOp::Add,
        cf_override: None,
    };
    let mut lazy = Cpu386 {
        pending_flags: PendingFlags::from_legacy(&lf),
        ..Default::default()
    };
    assert_eq!(
        lazy.eflags(),
        eager.registers.eflags,
        "materialized whole eflags must match eager"
    );
    lazy.materialize_flags();
    assert!(lazy.pending_flags.is_none());
    assert_eq!(lazy.registers.eflags, eager.registers.eflags);
}

#[test]
fn alu_logic_defers_flags_and_preserves_aux() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_AF | FLAG_CF | FLAG_OF, true);
    let result = cpu.alu(4, 0xf0, 0x0f, BusWidth::Byte);
    assert_eq!(result, 0);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "logic flags stay lazy"
    );
    assert!(!cpu.flag(FLAG_CF));
    assert!(!cpu.flag(FLAG_OF));
    assert!(cpu.flag(FLAG_ZF));
    assert!(cpu.flag(FLAG_AF), "AF remains the previous undefined value");
    cpu.materialize_flags();
    assert_eq!(cpu.registers.eflags & (FLAG_CF | FLAG_OF), 0);
    assert_ne!(cpu.registers.eflags & FLAG_AF, 0);
}

#[test]
fn inc_dec_defers_flags_while_preserving_carry() {
    let mut cpu = Cpu386::default();
    cpu.set_flag(FLAG_CF, true);
    let result = cpu.inc_dec(0xffff, false, BusWidth::Word);
    assert_eq!(result, 0);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "INC should not materialize just to keep CF"
    );
    assert!(cpu.flag(FLAG_CF), "INC preserves CF");
    assert!(cpu.flag(FLAG_ZF));

    cpu.set_flag(FLAG_CF, false);
    let result = cpu.inc_dec(0, true, BusWidth::Byte);
    assert_eq!(result, 0xff);
    assert!(
        cpu.pending_flags.tag & (1u32 << 31) != 0,
        "DEC should stay lazy"
    );
    assert!(!cpu.flag(FLAG_CF), "DEC preserves CF");
    assert!(cpu.flag(FLAG_SF));
}

#[test]
fn shift_after_pending_flags_matches_materialized_without_materializing() {
    for &(op, value, count) in &[
        (4, 0x4000, 1), // SHL defines OF
        (4, 0x0001, 2), // SHL preserves previous OF for multi-bit counts
        (5, 0x8001, 2), // SHR
        (7, 0x8001, 2), // SAR
    ] {
        let mut expected = Cpu386::default();
        expected.alu_add(0x7f, 0x01, 0, BusWidth::Byte); // pending OF+AF
        expected.materialize_flags();
        let expected_result = expected.shift_rotate(op, value, count, BusWidth::Word);
        let expected_flags = expected.eflags();

        let mut lazy = Cpu386::default();
        lazy.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
        lazy.reset_perf_counters();
        let lazy_result = lazy.shift_rotate(op, value, count, BusWidth::Word);

        assert_eq!(lazy_result, expected_result, "op={op} count={count}");
        assert_eq!(lazy.eflags(), expected_flags, "op={op} count={count}");
        assert_eq!(lazy.perf.flag_materializations, 0, "op={op} count={count}");
        assert!(lazy.pending_flags.is_none(), "op={op} count={count}");
    }

    let mut expected = Cpu386::default();
    expected.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
    expected.materialize_flags();
    let expected_result = expected.double_shift(true, 0x0001, 0, 2, OperandSize::Word);
    let expected_flags = expected.eflags();

    let mut lazy = Cpu386::default();
    lazy.alu_add(0x7f, 0x01, 0, BusWidth::Byte);
    lazy.reset_perf_counters();
    let lazy_result = lazy.double_shift(true, 0x0001, 0, 2, OperandSize::Word);

    assert_eq!(lazy_result, expected_result);
    assert_eq!(lazy.eflags(), expected_flags);
    assert_eq!(lazy.perf.flag_materializations, 0);
    assert!(lazy.pending_flags.is_none());
}

#[test]
fn fp_timing_identity_does_not_change_fpu_clocks() {
    // FADD ST,ST(1) is opcode D8 C1 (register form: D8 /0, modrm=C1 → mod=3, reg=0, rm=1).
    // The FPU executor charges 20 raw clocks for a register-form arithmetic op
    // (fpu_reg_arith_st0 returns clocks(20)). With fp_timing==(1,1) the identity
    // scale_fp_clocks call must return 20 unchanged, so elapsed_clocks after one cycle
    // at I486 must equal elapsed_clocks at I586 — proving the FP factor is truly identity
    // and does not disturb the existing level_timing scaling.
    //
    // We also push 1.0 into ST0 and ST1 first so FADD does not trap on an empty stack;
    // that means we run three cycles total (FLD1; FLD1; FADD ST,ST(1)) and then measure.
    // But to isolate just the FADD clock charge, we record elapsed_clocks before and after
    // the FADD cycle at each level.
    let code: &[u8] = &[
        0xd9, 0xe8, // FLD1  → ST0 = 1.0
        0xd9, 0xe8, // FLD1  → push 1.0 again (ST0=1, ST1=1)
        0xd8, 0xc1, // FADD ST(0), ST(1)
    ];

    let fadd_elapsed = |level: CpuLevel| -> u64 {
        let (mut cpu, memory) = real_mode_cpu(code, 0x20);
        cpu.set_level(level);
        let mut bus = TestBus::with_memory(memory);
        // Execute FLD1; FLD1 to load the stack.
        cpu.cycle(&mut bus).unwrap();
        cpu.cycle(&mut bus).unwrap();
        // Snapshot before the FADD.
        let before = cpu.elapsed_clocks;
        cpu.cycle(&mut bus).unwrap();
        cpu.elapsed_clocks - before
    };

    let fadd_i486 = fadd_elapsed(CpuLevel::I486);
    let fadd_i586 = fadd_elapsed(CpuLevel::I586);

    // Both modes share level_timing (1,12); the per-class FP dial is identity at
    // I486 and Register-class x0.25 at I586 (P5 pairing/issue-rate honesty), so
    // the register FADD charge at 586 must be at most the 486 charge and both
    // must stay nonzero (the fractional carry may not round a cheap op to a
    // permanent zero).
    assert!(
        fadd_i586 <= fadd_i486,
        "per-class fp dial: register FADD at I586 ({fadd_i586}) must not exceed I486 ({fadd_i486})"
    );
    assert!(
        fadd_i486 > 0,
        "FADD must charge at least 1 scaled clock at I486 (got {fadd_i486})"
    );
}

// ---- V86 monitor test harness -------------------------------------------------
// Memory map (physical == linear, identity paged):
//   0x00000 IVT area; 0x01000 page directory; 0x02000 page table 0 (identity,
//   present+rw+user); 0x03000 GDT; 0x04000 IDT; 0x05000 TSS (+ I/O bitmap);
//   ESP0 = 0x07000 (ring-0 stack, flat SS base 0); 0x08000 monitor code;
//   V86 guest: SS=0x0900, CS=0x0A00 (code at phys 0xA000).
// GDT selectors: 0x08 ring0 code (32-bit), 0x10 ring0 data/stack, 0x18 TSS.
const GDT: u32 = 0x3000;
const IDT: u32 = 0x4000;
const TSS: u32 = 0x5000;
const R0_CS: u16 = 0x08;
const R0_SS: u16 = 0x10;
const TSS_SEL: u16 = 0x18;
const MON_CODE: u32 = 0x8000;
const ESP0: u32 = 0x7000;

fn put32(m: &mut [u8], off: u32, v: u32) {
    m[off as usize..off as usize + 4].copy_from_slice(&v.to_le_bytes());
}
fn put16(m: &mut [u8], off: u32, v: u16) {
    m[off as usize..off as usize + 2].copy_from_slice(&v.to_le_bytes());
}
fn descriptor(base: u32, limit: u32, access: u8, gran: u8) -> [u8; 8] {
    let mut d = [0u8; 8];
    d[0..2].copy_from_slice(&(limit as u16).to_le_bytes());
    d[2..4].copy_from_slice(&(base as u16).to_le_bytes());
    d[4] = (base >> 16) as u8;
    d[5] = access;
    d[6] = ((limit >> 16) as u8 & 0x0f) | (gran & 0xf0);
    d[7] = (base >> 24) as u8;
    d
}
fn int_gate(m: &mut [u8], vector: u8, offset: u32) {
    let base = IDT + u32::from(vector) * 8;
    put16(m, base, offset as u16);
    put16(m, base + 2, R0_CS);
    m[base as usize + 4] = 0;
    m[base as usize + 5] = 0x8e; // present, DPL0, 32-bit interrupt gate
    put16(m, base + 6, (offset >> 16) as u16);
}
fn cpu_mem(bus: &TestBus, addr: u32) -> [u8; 4] {
    let a = addr as usize;
    [
        bus.memory[a],
        bus.memory[a + 1],
        bus.memory[a + 2],
        bus.memory[a + 3],
    ]
}

/// Build the world; CPU sits in protected mode + paging with TR/GDTR/IDTR loaded.
fn v86_world(monitor: &[u8], guest: &[u8], io_bitmap: &[u8]) -> (Cpu386, TestBus) {
    let mut m = vec![0u8; 0x20000];
    // Identity paging: PDE[0] -> PT at 0x2000; first 0x20 pages identity present+rw+user.
    put32(&mut m, 0x1000, 0x2000 | 0x7);
    for i in 0..0x20u32 {
        put32(&mut m, 0x2000 + i * 4, (i << 12) | 0x7);
    }
    // GDT: null (offset 0), ring0 code 0x9b (sel 0x08), ring0 data 0x93 (sel 0x10),
    // TSS 0x89 (sel 0x18).
    let d = descriptor(0, 0xfffff, 0x9b, 0xc0);
    m[(GDT + 0x08) as usize..(GDT + 0x08) as usize + 8].copy_from_slice(&d);
    let d = descriptor(0, 0xfffff, 0x93, 0xc0);
    m[(GDT + 0x10) as usize..(GDT + 0x10) as usize + 8].copy_from_slice(&d);
    let tss_limit = 0x68 + io_bitmap.len() as u32;
    let d = descriptor(TSS, tss_limit, 0x89, 0x00);
    m[(GDT + 0x18) as usize..(GDT + 0x18) as usize + 8].copy_from_slice(&d);
    // TSS: ESP0, SS0, I/O-map base (word at TSS+0x66), bitmap.
    put32(&mut m, TSS + 4, ESP0);
    put16(&mut m, TSS + 8, R0_SS);
    put16(&mut m, TSS + 0x66, 0x68);
    m[(TSS + 0x68) as usize..(TSS + 0x68) as usize + io_bitmap.len()].copy_from_slice(io_bitmap);
    // IDT: #GP (13) and INT 0x21 -> monitor.
    int_gate(&mut m, 13, MON_CODE);
    int_gate(&mut m, 0x21, MON_CODE);
    m[MON_CODE as usize..MON_CODE as usize + monitor.len()].copy_from_slice(monitor);
    m[0xA000..0xA000 + guest.len()].copy_from_slice(guest);

    let mut cpu = Cpu386::default();
    cpu.control.cr0 |= CR0_PE | CR0_PG;
    cpu.control.cr3 = 0x1000;
    cpu.gdtr.base = GDT;
    cpu.gdtr.limit = 0xff;
    cpu.idtr.base = IDT;
    cpu.idtr.limit = 0xfff;
    cpu.tr = SegmentRegister {
        selector: TSS_SEL,
        base: TSS,
        limit: tss_limit,
        access: 0x89,
        default_size_32: false,
    };
    let bus = TestBus::with_memory(m);
    (cpu, bus)
}

/// Put `cpu` into a V86 task at CS:IP=0x0A00:ip, SS:SP=0x0900:sp, IOPL 0.
/// DS/ES/FS/GS are seeded with sensible defaults; a caller may overwrite them
/// afterward to probe the V86 segment frame (none of them are load-bearing here).
fn enter_v86_direct(cpu: &mut Cpu386, ip: u32, sp: u32) {
    cpu.registers.eflags = (cpu.registers.eflags & !0x3000) | FLAG_VM | 0x2;
    cpu.registers.eip = ip;
    cpu.registers.set_esp(sp);
    cpu.load_segment_real(SegmentIndex::Cs, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Ss, 0x0900);
    cpu.load_segment_real(SegmentIndex::Ds, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Es, 0x0A00);
    cpu.load_segment_real(SegmentIndex::Fs, 0);
    cpu.load_segment_real(SegmentIndex::Gs, 0);
    // This helper sets EFLAGS.VM directly (no IRET/task-switch transition runs), so
    // the cached `cpl` must be seeded to the fixed V86 level by hand, same as a real
    // transition would leave it.
    cpu.cpl = 3;
}

#[test]
fn deliver_exception_from_v86_builds_the_v86_frame_on_ring0_stack() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.load_segment_real(SegmentIndex::Ds, 0x1111);
    cpu.load_segment_real(SegmentIndex::Es, 0x2222);
    cpu.load_segment_real(SegmentIndex::Fs, 0x3333);
    cpu.load_segment_real(SegmentIndex::Gs, 0x4444);
    let saved_eflags = cpu.registers.eflags;

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, R0_SS);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0);
    let esp = cpu.registers.esp();
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, esp + o));
    // From the handler's ESP upward: [err], EIP, CS, EFLAGS, ESP, SS, ES, DS, FS, GS.
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(rd(4), 0x10, "V86 EIP");
    assert_eq!(rd(8) & 0xffff, 0x0A00, "V86 CS");
    assert_eq!(rd(12) & FLAG_VM, FLAG_VM, "pushed EFLAGS carries VM=1");
    assert_eq!(rd(12), saved_eflags, "pushed EFLAGS is the pre-clear image");
    assert_eq!(rd(16), 0x1000, "V86 ESP");
    assert_eq!(rd(20) & 0xffff, 0x0900, "V86 SS");
    assert_eq!(rd(24) & 0xffff, 0x2222, "V86 ES");
    assert_eq!(rd(28) & 0xffff, 0x1111, "V86 DS");
    assert_eq!(rd(32) & 0xffff, 0x3333, "V86 FS");
    assert_eq!(rd(36) & 0xffff, 0x4444, "V86 GS");
}

#[test]
fn deliver_exception_onto_a_16bit_ring0_stack_wraps_sp_and_preserves_high_esp() {
    // The exact fault scenario this task fixes: a 32-bit interrupt gate delivers
    // onto a ring-0 stack whose SS descriptor has B=0 (a 16-bit stack segment,
    // as DOS4GW/VCPI clients use). 386 PRM 17-43/17-74: "Load new SS:eSP value
    // from TSS" is B-keyed (17-12) -- a B=0 target stack takes the TSS value
    // into SP only, and ESP's high word carries over from the interrupted
    // context untouched. The V86 interrupt frame (10 dwords) is then built at
    // SP-wrapped addresses, only SP advancing.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Flip the ring-0 data descriptor's B bit off (byte 6 bit 6 = 0x40). Give
    // TSS ESP0 a nonzero high word (0x0001) to prove it is dropped (SP-only
    // load), and enter V86 with ESP high word 0 so a leftover-high-word bug
    // would be visible in the final ESP.
    bus.memory[(GDT + 0x10 + 6) as usize] &= !0x40;
    put32(&mut bus.memory, TSS + 4, 0x0001_0010);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.stack_is_32bit(), "the loaded SS0 must carry B=0");
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.esp(),
        0x0000_ffe8,
        "SP takes only the TSS's low 16 bits, then wraps at the 16-bit \
             boundary; the interrupted context's ESP high word (0) carries over, \
             not the TSS's high word (0x0001)"
    );
    // The frame lives at SS0.base (0) + the wrapped 16-bit SP (0xffe8).
    let rd = |o: u32| u32::from_le_bytes(cpu_mem(&bus, 0xffe8 + o));
    assert_eq!(rd(0), 0, "error code");
    assert_eq!(rd(4), 0x10, "V86 EIP");
    assert_eq!(rd(16), 0x1000, "V86 ESP");
}

#[test]
fn deliver_exception_onto_a_16bit_ring0_stack_preserves_interrupted_esp_high_word() {
    // Companion to the case above: this time the interrupted V86 context's ESP
    // has a nonzero high word, proving it survives the SP-only TSS load (rather
    // than being replaced by the TSS's, or zeroed).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    bus.memory[(GDT + 0x10 + 6) as usize] &= !0x40;
    put32(&mut bus.memory, TSS + 4, 0x0000_0010);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    cpu.registers.set_esp(0xbeef_1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    assert!(!cpu.stack_is_32bit(), "the loaded SS0 must carry B=0");
    assert_eq!(
        cpu.registers.esp(),
        0xbeef_ffe8,
        "the interrupted context's ESP high word (0xbeef) must carry over \
             onto the new B=0 stack, with SP taken from the TSS and then wrapped"
    );
}

#[test]
fn v86_external_interrupt_on_vector_8_pushes_no_error_code() {
    // A real DOS boot under a V86 monitor keeps the PIC at base 0x08, so IRQ0
    // lands on vector 8 (#DF). An EXTERNAL interrupt must NOT push an error code
    // even there — only a genuine CPU exception does. (is_external = true.)
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    int_gate(&mut bus.memory, 8, MON_CODE);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 8, None, true).unwrap();

    // In the monitor: the top of the ring-0 stack is the V86 EIP, not an error
    // code (the frame is EIP, CS, EFLAGS, ... with no error code beneath EIP).
    let esp = cpu.registers.esp();
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        u32::from_le_bytes(cpu_mem(&bus, esp)),
        0x10,
        "external interrupt on vector 8 must not push an error code"
    );
}

#[test]
fn iret_into_v86_restores_the_task() {
    // Monitor at CPL0 with a V86 return frame on its stack; IRET must re-enter V86.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // Build the 32-bit V86 IRET frame (push high-to-low): GS,FS,DS,ES,SS,ESP,EFLAGS,CS,EIP.
    let vm_eflags = FLAG_VM | 0x2;
    for v in [
        0x4444u32, 0x3333, 0x1111, 0x2222, 0x0900, 0x1000, vm_eflags, 0x0A00, 0x0010,
    ] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }

    cpu.iret(&mut bus, OperandSize::Dword).unwrap();

    assert!(cpu.is_v86_mode(), "IRET with popped VM=1 must re-enter V86");
    assert_eq!(cpu.registers.eip, 0x0010);
    assert_eq!(cpu.registers.cs().selector, 0x0A00);
    assert_eq!(
        cpu.registers.cs().base,
        0x0A00 << 4,
        "real-mode base=sel<<4"
    );
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, 0x0900);
    assert_eq!(cpu.registers.esp(), 0x1000);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ds).selector, 0x1111);
    assert_eq!(cpu.registers.segment(SegmentIndex::Es).selector, 0x2222);
    assert_eq!(cpu.registers.segment(SegmentIndex::Fs).selector, 0x3333);
    assert_eq!(cpu.registers.segment(SegmentIndex::Gs).selector, 0x4444);
    assert_eq!(cpu.current_privilege_level(), 3, "V86 is always CPL 3");
}

#[test]
fn iret_into_v86_with_dirty_high_word_eip_faults_before_committing_v86_state() {
    // Same 32-bit V86 IRET frame as `iret_into_v86_restores_the_task`, but the popped
    // EIP carries a nonzero high word (0x0001_0000). 386 PRM STACK-RETURN-TO-V86 checks
    // "instruction pointer not within code segment limit" against the popped EIP and
    // raises #GP(0) *before* EFLAGS/CS/EIP/ESP or the V86 data segments are committed --
    // ahead of every `Pop()` in the pseudocode's V86-tail sequence. A V86 CS is always a
    // 16-bit real-mode-style segment (fixed 0xffff limit), so this EIP is always out of
    // range: `iret` must return the fault directly, leaving the ring-0 monitor's own
    // CS/EIP/segments untouched (as if the IRET itself never executed), not commit a
    // fabricated V86 frame and only discover the violation on the next fetch.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    let vm_eflags = FLAG_VM | 0x2;
    for v in [
        0x4444u32,
        0x3333,
        0x1111,
        0x2222,
        0x0900,
        0x1000,
        vm_eflags,
        0x0A00,
        0x0001_0000,
    ] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }

    let result = cpu.iret(&mut bus, OperandSize::Dword);

    assert!(
        matches!(
            result,
            Err(InternalFault::Exception {
                vector: 13,
                error_code: Some(0),
            })
        ),
        "out-of-limit popped EIP must fault #GP(0) directly from iret: {result:?}"
    );
    assert!(
        !cpu.is_v86_mode(),
        "a faulted IRET must not have entered V86"
    );
    assert_eq!(
        cpu.registers.cs().selector,
        R0_CS,
        "the monitor's own CS must be untouched by the faulted IRET"
    );
    // 9 dwords were pushed to build the frame; the faulted IRET must restore ESP to
    // that pre-IRET value (finish_instruction rewinds only EIP/CS, so iret itself
    // must undo its three pops or the monitor's stack drifts 12 bytes per fault).
    assert_eq!(
        cpu.registers.esp(),
        0x6800 - 9 * 4,
        "a faulted IRET must leave ESP exactly pre-IRET"
    );
}

#[test]
fn iret_inter_privilege_return_to_ring3() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Ring-3 code (access 0xfb) + data (0xf3) at GDT slots 0x20 / 0x28.
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16; // 0x20 | RPL3
    let r3_ss = 0x2Bu16; // 0x28 | RPL3
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.registers.set_esp(0x6800);
    // Inter-privilege IRET frame (high-to-low): SS, ESP, EFLAGS, CS, EIP.
    for v in [u32::from(r3_ss), 0x2000, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert!(!cpu.is_v86_mode());
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, r3_ss);
    assert_eq!(cpu.registers.esp(), 0x2000);
}

#[test]
fn iret_to_outer_ring_nulls_data_segments_inaccessible_at_the_new_cpl() {
    // 386 PRM (IRET, return to outer privilege level): each of DS/ES/FS/GS
    // holding a data or non-conforming code segment with DPL < new CPL is
    // loaded with the null selector. Borland's DPMI32VM relies on this: its
    // ring-0 trap handler IRETDs to ring 3 with DS still holding the ring-0
    // data selector; ring-3 code then PUSH/POPs DS, which only works if the
    // return nulled it (popping a DPL-0 selector at CPL 3 is #GP).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xfffff, 0xf3, 0xc0);
    // Conforming ring-0 code (access 0x9f): readable at any CPL, must survive.
    let r0_conforming = descriptor(0, 0xfffff, 0x9f, 0xc0);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    bus.memory[(GDT + 0x30) as usize..(GDT + 0x30) as usize + 8].copy_from_slice(&r0_conforming);
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ds, R0_SS).unwrap(); // ring-0 data
    cpu.load_segment(&mut bus, SegmentIndex::Es, 0x2B).unwrap(); // ring-3 data
    cpu.load_segment(&mut bus, SegmentIndex::Fs, R0_SS).unwrap(); // ring-0 data
    cpu.load_segment(&mut bus, SegmentIndex::Gs, 0x33).unwrap(); // conforming r0 code
    cpu.registers.set_esp(0x6800);
    for v in [0x2Bu32, 0x2000, 0x2, 0x23, 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3);
    let sel = |cpu: &Cpu386, s| cpu.registers.segment(s).selector;
    assert_eq!(sel(&cpu, SegmentIndex::Ds), 0, "ring-0 DS nulled");
    assert_eq!(sel(&cpu, SegmentIndex::Fs), 0, "ring-0 FS nulled");
    assert_eq!(sel(&cpu, SegmentIndex::Es), 0x2B, "ring-3 ES survives");
    assert_eq!(
        sel(&cpu, SegmentIndex::Gs),
        0x33,
        "conforming code GS survives"
    );
}

#[test]
fn iret_inter_privilege_return_to_a_16bit_stack_wraps_sp_and_preserves_high_esp() {
    // 386 PRM 17-80: "Load SS:eSP from stack" is B-keyed (17-12). Returning to
    // an outer ring whose SS descriptor has B=0 (the DPMI/DOS-extender 16-bit
    // stack shape) must take the popped value into SP only, wrap at the
    // 16-bit boundary, and leave ESP's high word as the inner stack's --
    // exactly the documented real-silicon ESP-high-word leak on a 16-bit ring
    // transition.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // Ring-3 code (access 0xfb) + a B=0 (16-bit) ring-3 data descriptor (0xf3,
    // flags byte with the B bit, 0x40, cleared) at GDT slots 0x20 / 0x28.
    let r3_code = descriptor(0, 0xfffff, 0xfb, 0xc0);
    let r3_data = descriptor(0, 0xffff, 0xf3, 0x00);
    bus.memory[(GDT + 0x20) as usize..(GDT + 0x20) as usize + 8].copy_from_slice(&r3_code);
    bus.memory[(GDT + 0x28) as usize..(GDT + 0x28) as usize + 8].copy_from_slice(&r3_data);
    let r3_cs = 0x23u16; // 0x20 | RPL3
    let r3_ss = 0x2Bu16; // 0x28 | RPL3
    cpu.registers.eflags = 0x2;
    cpu.load_segment(&mut bus, SegmentIndex::Cs, R0_CS).unwrap();
    cpu.load_segment(&mut bus, SegmentIndex::Ss, R0_SS).unwrap();
    // Low half of ESP (0x6800) is the address `push` actually uses (the
    // inner stack is B=1, so it addresses with full ESP); the high half
    // (0x0001) must not leak onto the B=0 outer stack after IRET, and the
    // physical address stays within the test's identity-mapped 0x20000
    // bytes (0x0001_6800 < 0x20000).
    cpu.registers.set_esp(0x0001_6800);
    // Popped ESP has a different nonzero high word (0x0002); a B=0 target
    // stack must drop it (SP-only load), not adopt it.
    for v in [u32::from(r3_ss), 0x0002_0010, 0x2, u32::from(r3_cs), 0x1234] {
        cpu.push(&mut bus, v, OperandSize::Dword).unwrap();
    }
    cpu.iret(&mut bus, OperandSize::Dword).unwrap();
    assert_eq!(cpu.current_privilege_level(), 3, "returned to ring 3");
    assert!(!cpu.stack_is_32bit(), "the loaded outer SS must carry B=0");
    assert_eq!(cpu.registers.eip, 0x1234);
    assert_eq!(cpu.registers.cs().selector, r3_cs);
    assert_eq!(cpu.registers.segment(SegmentIndex::Ss).selector, r3_ss);
    assert_eq!(
        cpu.registers.esp(),
        0x0001_0010,
        "SP takes the popped value's low 16 bits; ESP's high word carries \
             over from the inner stack (0x0001), not the popped high word \
             (0x0002)"
    );
}

#[test]
fn v86_out_consults_the_io_permission_bitmap() {
    // Guest at 0x0A00:0 does `OUT 0x21, AL` (E6 21). Bitmap traps port 0x21.
    let mut bitmap = vec![0u8; 0x20 + 1]; // ports 0..0x100 + terminator byte
    bitmap[0x21 / 8] |= 1 << (0x21 % 8);
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);
    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "trapped OUT must land in the ring-0 monitor"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

#[test]
fn v86_out_to_a_permitted_port_runs_the_io() {
    let bitmap = vec![0u8; 0x20 + 1]; // all-zero: everything permitted
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    cpu.cycle(&mut bus).unwrap();
    assert!(cpu.is_v86_mode(), "permitted OUT stays in V86");
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite && c.address == 0x21),
        "permitted OUT should reach the I/O bus"
    );
}

#[test]
fn v86_monitor_round_trip_go_no_go() {
    // Guest: STI (fb) ; OUT 0x80,AL (e6 80) ; INT 0x21 (cd 21) ; HLT (f4).
    // HLT is now privileged (require_cpl0): a V86 task is always CPL 3, so the
    // guest's HLT traps into the monitor exactly like STI and INT 0x21 rather
    // than halting the machine directly. The monitor emulates it by advancing
    // past the F4 byte and halting for real at ring 0 (mirroring TOKAEMM's
    // `.hlt` handler in tokaemm.asm): that real HLT is what stops the machine,
    // observed here as `outcome.halted` while CS is still the ring-0 monitor
    // selector (not while `cpu.is_v86_mode()`, since the guest itself never
    // executes HLT to completion anymore).
    let guest = [0xfb, 0xe6, 0x80, 0xcd, 0x21, 0xf4];
    let monitor = [0xf4]; // unused: we emulate the monitor from Rust below.
    let bitmap = vec![0u8; 0x20 + 1]; // all-zero: ports 0..0x100 permitted (+ terminator byte)
    let (mut cpu, mut bus) = v86_world(&monitor, &guest, &bitmap);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let mut traps = 0;
    let mut monitor_halted = false;
    for _ in 0..64 {
        let outcome = cpu.cycle(&mut bus).unwrap();
        if !cpu.is_v86_mode() && cpu.registers.cs().selector == R0_CS {
            if outcome.halted {
                // The monitor's HLT-emulation path ran its own real HLT at ring 0
                // to idle the machine on the guest's behalf; nothing left to IRET.
                monitor_halted = true;
                break;
            }
            // In the monitor because the guest faulted. Read the V86 #GP(13) frame,
            // advance the guest EIP past the faulting instruction, IRET back to V86.
            // STI, INT 0x21, and now HLT all arrive here as #GP(13): each is either
            // IOPL-sensitive (check_v86_iopl) or CPL-sensitive (require_cpl0) and a
            // V86 task always runs at IOPL 0 / CPL 3. INT 0x21 does NOT dispatch
            // through its own IDT gate (it is intercepted before delivery), so every
            // trap in this test is vector 13.
            traps += 1;
            // Discard the error code (vector 13 pushes one) so IRET pops from EIP.
            // Frame layout from the handler's ESP upward is [err], EIP, CS, ... (see the
            // sibling deliver_exception test); after skipping the 4-byte error code the
            // V86 EIP is at the top of stack, so cpu_mem(&bus, esp) reads it directly.
            let esp = cpu.registers.esp() + 4;
            cpu.registers.set_esp(esp);
            let guest_eip = u32::from_le_bytes(cpu_mem(&bus, esp));
            // The guest is loaded at phys 0xA000 == V86 CS(0x0A00) << 4, so guest_eip
            // (a segment offset) indexes the guest code bytes directly. This literal
            // tracks v86_world's guest load base and enter_v86_direct's V86 CS.
            let opcode = bus.memory[(0xA000 + guest_eip) as usize];
            let len = match opcode {
                0xfb => 1, // STI
                0xcd => 2, // INT imm8
                0xf4 => {
                    // HLT: the guest's virtual IF is set (STI already ran), so a
                    // faithful monitor would really halt here on the guest's behalf
                    // (tokaemm.asm's `.hlt` runs `sti; hlt` at ring 0) rather than
                    // resuming V86. This Rust stand-in for the monitor halts the CPU
                    // directly instead of round-tripping through an IRET into V86
                    // followed immediately by a real HLT trap: same observable
                    // result (the machine halts with CS still the monitor selector),
                    // fewer moving parts in the harness.
                    cpu.halted = true;
                    continue;
                }
                other => {
                    panic!("unexpected trap on opcode {other:#x} at guest eip {guest_eip:#x}")
                }
            };
            bus.memory[esp as usize..esp as usize + 4]
                .copy_from_slice(&(guest_eip + len).to_le_bytes());
            cpu.iret(&mut bus, OperandSize::Dword).unwrap();
            continue;
        }
    }

    assert!(
        monitor_halted,
        "the monitor never halted on the guest's HLT"
    );
    assert_eq!(traps, 3, "STI, INT 0x21, and HLT must each trap once");
    assert!(
        bus.trace
            .cycles()
            .iter()
            .any(|c| c.kind == BusAccessKind::IoWrite && c.address == 0x80),
        "permitted OUT 0x80 should have run in V86"
    );
}

// ---- Non-identity-mapped system structures (translate_linear_system) ----------
//
// `v86_world`'s page tables are identity-mapped, so TSS/GDT/IDT linear == physical
// there and every system-structure read would look correct even with the raw,
// unpaged `bus.read_memory` these tests were written to catch (the JEMMEX bug:
// its monitor sits at a high linear alias -- e.g. 0xf8017000 -- of low physical
// RAM). These tests add a *second* PDE mapping a high linear window onto the same
// physical TSS/GDT page, then address the TSS/GDT only through that alias, so a
// regression back to raw `bus.read_memory(self.tr.base + ..)` reads unmapped
// physical memory (or, in TestBus, the wrong bytes) instead of the real fields.

/// Linear window aliasing the TSS's physical page one PDE slot up (JEMMEX-style
/// high monitor mapping): PDE[1] -> the same page table as PDE[0], so linear
/// 0x00400000 + phys(0..0x1000) reads/writes the identical physical bytes as the
/// identity mapping at phys directly.
const ALIAS_BASE: u32 = 0x0040_0000;

/// Extend `v86_world`'s page directory with a second PDE (index 1) pointing at
/// the same page table as PDE[0], then move the TSS to be addressed only through
/// the alias: `cpu.tr.base` and the TSS GDT descriptor's base are both set to
/// `ALIAS_BASE + TSS`, while the bytes still live at physical `TSS`. A test that
/// reads/writes the TSS via a raw, unpaged `bus.read_memory(self.tr.base + ..)`
/// would touch physical `ALIAS_BASE + TSS` (zeroed, wrong data) instead of the
/// real TSS at physical `TSS`.
fn alias_tss_through_second_pde(bus: &mut TestBus, cpu: &mut Cpu386) {
    // PDE[1] (linear 0x0040_0000..0x0080_0000) -> the same PT as PDE[0].
    put32(&mut bus.memory, 0x1000 + 4, 0x2000 | 0x7);
    let tss_limit = cpu.tr.limit;
    cpu.tr.base = ALIAS_BASE + TSS;
    // Repoint the TSS GDT descriptor's base at the alias too, so LTR-style
    // re-reads and `set_tss_busy`'s GDT access-byte patch land on the alias.
    let d = descriptor(ALIAS_BASE + TSS, tss_limit, 0x89, 0x00);
    bus.memory[(GDT + 0x18) as usize..(GDT + 0x18) as usize + 8].copy_from_slice(&d);
}

#[test]
fn deliver_exception_from_v86_with_cs_rpl3_does_not_fault_the_monitors_own_pushes() {
    // Dossier reproduction: a V86 source whose CS selector carries RPL bits == 3
    // (the DOS HMA stub lives at 0xFFFF, reached via an XMS chain-through) must not
    // make `deliver_exception`'s own ring-0 stack pushes look like a user access.
    // Before the fix, `current_privilege_level` derived "user" live from
    // `CS.selector & 3` -- read at the moment of the push, i.e. still the V86
    // source's arbitrary CS -- so a supervisor-only ESP0 page spuriously #PF'd on
    // the monitor's own frame-push, with CR2 landing on the stack pointer itself.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // The frame's pushes land BELOW ESP0 (0x7000), on page 6 (0x6000..0x6FFF), not
    // ESP0's own page: make that page supervisor-only (present+rw, U/S=0, dropping
    // the 0x4 user bit `v86_world` sets by default).
    put32(&mut bus.memory, 0x2000 + 6 * 4, 0x6000 | 0x3);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);
    // Load a V86 CS whose low bits are 3 -- a real-mode-style segment, so this is
    // legal V86 state, just an unusual selector value (0xFFFF, the HMA stub).
    cpu.load_segment_real(SegmentIndex::Cs, 0xffff);

    let result = cpu.deliver_exception(&mut bus, 13, Some(0), false);

    assert!(
        result.is_ok(),
        "the monitor's own supervisor-stack pushes must not spuriously #PF: {result:?}"
    );
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        R0_SS,
        "entry crossed to the ring-0 stack despite the V86 source CS's RPL bits"
    );
}

#[test]
fn nested_fault_during_delivery_reports_truthfully_not_as_idt_limit() {
    // Companion to the reproduction above: when delivery genuinely nests a fault
    // (here, ESP0's page is marked NOT PRESENT, so the frame push itself raises
    // #PF), `cycle`'s error mapping must surface `NestedFaultDuringDelivery` with
    // both vectors, not relabel it as a fabricated `IdtLimit` on the ORIGINAL vector
    // (the pre-fix behavior, which discarded the nested vector entirely).
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // The frame's pushes land on page 6 (0x6000..0x6FFF), just below ESP0: clear
    // that page's present bit entirely.
    put32(&mut bus.memory, 0x2000 + 6 * 4, 0x6000 | 0x6);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    let outer = cpu.deliver_exception(&mut bus, 13, Some(0), false);
    let inner_fault = outer.expect_err("the not-present ESP0 push must nest a fault");
    let InternalFault::Exception {
        vector: nested_vector,
        ..
    } = inner_fault
    else {
        panic!("expected a nested processor exception, got {inner_fault:?}");
    };
    assert_eq!(nested_vector, 14, "the nested fault is the write's own #PF");

    // Drive the same scenario through `cycle`'s public error mapping (the call site
    // this bug actually lived in) by raising vector 13 as the guest's own delivered
    // exception via a HLT that is not privileged in V86 IOPL<3 -- reuse
    // deliver_exception directly through the same `finish_instruction` tail instead,
    // since that IS the call site under test (see `finish_instruction`).
    let (mut cpu2, mut bus2) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    put32(&mut bus2.memory, 0x2000 + 6 * 4, 0x6000 | 0x6);
    enter_v86_direct(&mut cpu2, 0x10, 0x1000);
    let start_eip = cpu2.registers.eip;
    let start_cs = cpu2.registers.cs().selector;
    let result: Result<CycleOutcome, CpuError> = cpu2.finish_instruction(
        &mut bus2,
        Err(InternalFault::Exception {
            vector: 13,
            error_code: Some(0),
        }),
        start_eip,
        start_cs,
        None,
        None,
    );
    assert_eq!(
        result,
        Err(CpuError::NestedFaultDuringDelivery {
            original_vector: 13,
            nested_vector: 14,
        }),
        "{result:?}"
    );
}

#[test]
fn deliver_exception_from_v86_reads_esp0_ss0_through_a_non_identity_tss_mapping() {
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    alias_tss_through_second_pde(&mut bus, &mut cpu);
    enter_v86_direct(&mut cpu, 0x10, 0x1000);

    cpu.deliver_exception(&mut bus, 13, Some(0), false).unwrap();

    // ESP0/SS0 came from the TSS at its aliased linear address, not from
    // unmapped physical memory at ALIAS_BASE + TSS + 4/+8 (which is zeroed).
    assert!(!cpu.is_v86_mode(), "VM must be cleared on monitor entry");
    assert_eq!(cpu.registers.cs().selector, R0_CS);
    assert_eq!(cpu.registers.eip, MON_CODE);
    assert_eq!(
        cpu.registers.segment(SegmentIndex::Ss).selector,
        R0_SS,
        "SS0 must come from the TSS through the paged (aliased) address"
    );
    // ESP0 from the TSS, minus the 10-dword V86 interrupt frame (err code, EIP,
    // CS, EFLAGS, ESP, SS, ES, DS, FS, GS) pushed onto the new stack.
    assert_eq!(
        cpu.registers.esp(),
        ESP0 - 40,
        "ESP0 must come from the TSS through the paged (aliased) address"
    );
}

#[test]
fn ltr_loads_a_gdt_tss_descriptor_through_a_non_identity_mapping() {
    // Put the GDT itself behind the alias: GDT descriptors are read via
    // `read_gdt_descriptor` -> `read_system_linear_u32`, so aliasing the GDT's
    // page (not just the TSS's) exercises that path directly.
    let (mut cpu, mut bus) = v86_world(&[0xf4], &[0xf4], &[0x00]);
    // PDE[1] -> the same PT as PDE[0] (GDT/TSS both live in the identity-mapped
    // low pages, so one alias PDE covers both).
    put32(&mut bus.memory, 0x1000 + 4, 0x2000 | 0x7);
    cpu.gdtr.base = ALIAS_BASE + GDT;
    cpu.registers.eflags = 0x2; // ring 0, no VM/IOPL surprises

    cpu.load_tr(&mut bus, TSS_SEL).unwrap();

    assert_eq!(cpu.tr.selector, TSS_SEL);
    assert_eq!(
        cpu.tr.base, TSS,
        "LTR must decode the TSS descriptor's base field from the aliased GDT"
    );
    assert_eq!(
        cpu.tr.access & 0x02,
        0x02,
        "LTR must mark the TSS busy in the cached descriptor"
    );
    // The busy bit patch-back must land on the real (aliased) GDT byte, not on
    // unmapped physical memory.
    let access_byte = bus.memory[(GDT + 0x18 + 5) as usize];
    assert_eq!(
        access_byte & 0x02,
        0x02,
        "GDT busy bit must be set in place"
    );
}

#[test]
fn v86_io_bitmap_check_reads_through_a_non_identity_mapped_tss() {
    // Bitmap traps port 0x21, but the TSS (and its I/O-map base word / bitmap
    // bytes) is only reachable through the ALIAS_BASE linear window. A raw,
    // unpaged read of `self.tr.base + 0x66` would read zeroed physical memory
    // at ALIAS_BASE + TSS + 0x66 and see io_base = 0 with an all-zero bitmap,
    // wrongly permitting the OUT.
    let mut bitmap = vec![0u8; 0x20 + 1];
    bitmap[0x21 / 8] |= 1 << (0x21 % 8);
    let guest = [0xe6, 0x21, 0xf4]; // out 0x21, al ; hlt
    let (mut cpu, mut bus) = v86_world(&[0xf4], &guest, &bitmap);
    alias_tss_through_second_pde(&mut bus, &mut cpu);
    enter_v86_direct(&mut cpu, 0, 0x1000);

    let outcome = cpu.cycle(&mut bus);

    assert!(outcome.is_ok(), "{outcome:?}");
    assert!(
        !cpu.is_v86_mode(),
        "the I/O-bitmap trap must be read through the aliased TSS mapping"
    );
    assert_eq!(cpu.registers.cs().selector, R0_CS);
}

/// Differential tests for the compiled loop-region (spike 4): every observable the
/// interpreter produces (architectural state, bus trace, clock totals, perf attribution)
/// must be byte-identical with the region admitted. The guest program is the exact
/// R_DrawColumn shape from the Doom census, relocated to low memory.
#[cfg(feature = "jit")]
mod jit_region {
    use super::*;

    const ENTRY: u32 = 0x100;
    const NOP_STARTER: u32 = 0xff;
    const COUNT_ADDR: usize = 0x400;
    const PATCHER: u32 = 0x140;
    const STEP_IMM: u32 = 0x0133_7c00;
    const PATCHED_IMM: u32 = 0x0066_7c00;

    /// The 51-byte R_DrawColumn loop, a HLT terminator at the fall-through, and the two-store
    /// self-patcher at 0x140 that rewrites both `add ebp,imm32` immediates exactly the way
    /// Doom's setup code does. The generic `build_block` must reproduce this exact 15-slot,
    /// self-loop shape (kinds + count) so this whole differential suite still holds.
    fn program() -> Vec<u8> {
        let mut m = vec![0u8; 0x1000];
        m[NOP_STARTER as usize] = 0x90;
        let loop_bytes: [u8; 0x33] = [
            0x8b, 0xcd, // mov ecx,ebp
            0x81, 0xc5, 0x00, 0x7c, 0x33, 0x01, // add ebp,STEP_IMM (imm at 0x104)
            0x88, 0x07, // mov [edi],al
            0xc1, 0xe9, 0x19, // shr ecx,25
            0x8b, 0xd5, // mov edx,ebp
            0x81, 0xc5, 0x00, 0x7c, 0x33, 0x01, // add ebp,STEP_IMM (imm at 0x111)
            0x88, 0x5f, 0x50, // mov [edi+0x50],bl
            0xc1, 0xea, 0x19, // shr edx,25
            0x8a, 0x04, 0x0e, // mov al,[esi+ecx]
            0x81, 0xc7, 0xa0, 0x00, 0x00, 0x00, // add edi,0xa0
            0x8a, 0x1c, 0x16, // mov bl,[esi+edx]
            0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [0x400]
            0x8a, 0x00, // mov al,[eax]
            0x8a, 0x1b, // mov bl,[ebx]
            0x75, 0xcd, // jnz ENTRY (rel8 -0x33)
        ];
        m[ENTRY as usize..ENTRY as usize + 0x33].copy_from_slice(&loop_bytes);
        m[0x133] = 0xf4; // HLT at the loop fall-through
        // Patcher: mov dword [0x104],PATCHED_IMM ; mov dword [0x111],PATCHED_IMM ; HLT.
        let p = PATCHER as usize;
        m[p..p + 10].copy_from_slice(&[0xc7, 0x05, 0x04, 0x01, 0x00, 0x00, 0x00, 0x7c, 0x66, 0x00]);
        m[p + 10..p + 20]
            .copy_from_slice(&[0xc7, 0x05, 0x11, 0x01, 0x00, 0x00, 0x00, 0x7c, 0x66, 0x00]);
        m[p + 20] = 0xf4;
        // Texture bytes at 0x300..0x380 (indexed by ebp>>25) and the colormap they point
        // into at 0x200..0x280 (the double indirection [eax]/[ebx] after AL/BL replace the
        // low byte of 0x200).
        for i in 0..0x80usize {
            m[0x300 + i] = 0x20 + (i as u8 & 0x1f);
            m[0x200 + i] = 0x80 ^ (i as u8);
        }
        // Real-mode IVT vector 13 (#GP) -> 0:0xB00, HLT handler (the fault test).
        m[13 * 4..13 * 4 + 2].copy_from_slice(&0x0b00u16.to_le_bytes());
        m[0xb00] = 0xf4;
        m
    }

    /// `program()` in a `size`-byte buffer, for loops that advance edi (stride 0xa0) past the
    /// 0x1000 the bare program occupies - hotness needs to run more iterations than that fits.
    fn program_in(size: usize) -> Vec<u8> {
        let mut m = vec![0u8; size];
        let p = program();
        m[..p.len()].copy_from_slice(&p);
        m
    }

    fn fresh_cpu(ds_limit: u32) -> Cpu386 {
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I586);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = true; // the shape is d=32 code
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        let mut ds = cpu.registers.segment(SegmentIndex::Ds);
        ds.limit = ds_limit;
        cpu.registers.set_segment(SegmentIndex::Ds, ds);
        cpu
    }

    /// Reset the guest to the canonical loop entry state with `count` iterations to run.
    fn arm_loop(cpu: &mut Cpu386, bus: &mut TestBus, count: u32) {
        cpu.registers.eip = NOP_STARTER;
        cpu.registers.set_esp(0x0700);
        cpu.write_gpr32(0, 0x200); // eax
        cpu.write_gpr32(1, 0); // ecx
        cpu.write_gpr32(2, 0); // edx
        cpu.write_gpr32(3, 0x200); // ebx
        cpu.write_gpr32(5, 0x0100_0000); // ebp
        cpu.write_gpr32(6, 0x300); // esi
        cpu.write_gpr32(7, 0x500); // edi
        bus.memory[COUNT_ADDR..COUNT_ADDR + 4].copy_from_slice(&count.to_le_bytes());
    }

    /// Drive `run_straight_line` (the machine batch seam) until a run halts. Returns the
    /// per-call scaled clock totals so cap-boundary shapes can be compared A/B too.
    fn drive_to_halt(cpu: &mut Cpu386, bus: &mut TestBus, cap: u64) -> Vec<(u32, u32)> {
        let mut calls = Vec::new();
        for _ in 0..10_000 {
            let outcome = cpu.run_straight_line(bus, cap).expect("no hard bus error");
            calls.push((outcome.core_clocks, cpu.registers.eip));
            if outcome.halted {
                return calls;
            }
        }
        panic!("guest never halted");
    }

    /// Warm both CPUs identically (fills the decode cache), admit + stamp the region on
    /// `jit` only, and assert the warm phases were identical.
    fn warm_and_admit(
        interp: &mut Cpu386,
        bus_i: &mut TestBus,
        jit: &mut Cpu386,
        bus_j: &mut TestBus,
    ) -> std::num::NonZeroU32 {
        arm_loop(interp, bus_i, 2);
        arm_loop(jit, bus_j, 2);
        drive_to_halt(interp, bus_i, u64::MAX);
        drive_to_halt(jit, bus_j, u64::MAX);
        assert_eq!(interp, jit, "warm phases must match before admission");
        let idx = jit::block::try_admit(jit, ENTRY, true)
            .expect("the warmed decode cache builds the drawcolumn block");
        let region = jit.jit_regions.get_mut(idx).unwrap();
        assert_eq!(region.ctx.slots[1].insn.imm, STEP_IMM);
        assert_eq!(region.ctx.slots[5].insn.imm, STEP_IMM);
        jit.decode_cache.stamp_region(ENTRY, true, idx);
        idx
    }

    fn assert_identical(interp: &Cpu386, bus_i: &TestBus, jit_cpu: &Cpu386, bus_j: &TestBus) {
        assert_eq!(interp, jit_cpu, "architectural + clock state diverged");
        assert_eq!(
            interp.elapsed_clocks, jit_cpu.elapsed_clocks,
            "elapsed guest clocks diverged"
        );
        assert_eq!(
            interp.timing_rem, jit_cpu.timing_rem,
            "scale remainder diverged"
        );
        assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
        assert_eq!(
            bus_i.trace.cycles(),
            bus_j.trace.cycles(),
            "bus cycle trace diverged"
        );
        let (pi, pj) = (interp.perf_counters(), jit_cpu.perf_counters());
        assert_eq!(
            pi.instructions, pj.instructions,
            "retired instruction count diverged"
        );
        assert_eq!(
            (pi.brk_cap, pi.brk_step, pi.brk_halt, pi.brk_interrupt),
            (pj.brk_cap, pj.brk_step, pj.brk_halt, pj.brk_interrupt),
            "run break attribution diverged"
        );
    }

    #[test]
    fn region_run_is_byte_identical_to_the_interpreter() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

        arm_loop(&mut interp, &mut bus_i, 8);
        arm_loop(&mut jit_cpu, &mut bus_j, 8);
        let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

        assert_eq!(calls_i, calls_j, "per-run outcomes diverged");
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        let perf = jit_cpu.perf_counters();
        assert!(perf.jit_region_entries > 0, "the region never executed");
        assert!(
            perf.jit_region_insns >= 8 * 15,
            "the region should have retired the loop's instructions, got {}",
            perf.jit_region_insns
        );
        assert_eq!(interp.perf_counters().jit_region_entries, 0);
    }

    #[test]
    fn region_breaks_at_the_interpreter_cap_boundary() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

        // Small caps force many mid-loop breaks; every break must land both executions on
        // the same eip with the same charged total (compared per call via drive_to_halt's
        // outcome log). Odd caps exercise the scale-remainder threading too.
        for cap in [7u64, 13, 50] {
            arm_loop(&mut interp, &mut bus_i, 14);
            arm_loop(&mut jit_cpu, &mut bus_j, 14);
            let calls_i = drive_to_halt(&mut interp, &mut bus_i, cap);
            let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, cap);
            assert_eq!(calls_i, calls_j, "cap {cap}: break boundaries diverged");
            assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        }
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }

    /// v2's inline slots (mov/add/shr) set gpr and flags natively; the brief flags
    /// flag-state equality after EVERY exit (incl. mid-iteration) as the hard correctness property.
    /// This test forces a cap-boundary exit at several points across the loop and compares the
    /// MATERIALIZED eflags (not just Cpu386 equality, but the actual `eflags()` value that
    /// resolves any pending descriptor the inline ADD left behind) between interpreter and JIT.
    /// A divergence here would mean the inline ADD's lazy descriptor or the inline SHR's eager
    /// materialization differs from the interpreter at the exit eip.
    #[test]
    fn region_inline_flag_state_matches_after_cap_exits() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
        // Run several iterations with caps that land exits at different slots, then compare the
        // materialized eflags at every break.
        for cap in [4u64, 8, 16, 31, 64, 100] {
            arm_loop(&mut interp, &mut bus_i, 14);
            arm_loop(&mut jit_cpu, &mut bus_j, 14);
            drive_to_halt(&mut interp, &mut bus_i, cap);
            drive_to_halt(&mut jit_cpu, &mut bus_j, cap);
            // The materialized eflags resolve any pending descriptor the inline add/shr left.
            assert_eq!(
                interp.eflags(),
                jit_cpu.eflags(),
                "cap {cap}: materialized eflags diverged after inline slots"
            );
            // And the raw pending-flag descriptor (if any) must match too.
            assert_eq!(
                interp.pending_flags, jit_cpu.pending_flags,
                "cap {cap}: pending flag descriptor diverged"
            );
        }
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }

    #[test]
    fn region_fault_mid_loop_delivers_identically() {
        // DS limit 0x5FF: the third iteration's `mov [edi],al` (edi = 0x640) raises #GP,
        // mid-region, on the write half of the unrolled pair. Both executions must rewind,
        // deliver through IVT 13, and halt in the handler with identical state.
        let mut interp = fresh_cpu(0x5ff);
        let mut jit_cpu = fresh_cpu(0x5ff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

        arm_loop(&mut interp, &mut bus_i, 100);
        arm_loop(&mut jit_cpu, &mut bus_j, 100);
        let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

        assert_eq!(calls_i, calls_j);
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        assert_eq!(
            jit_cpu.registers.eip, 0xb01,
            "both sides must halt inside the #GP handler"
        );
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }

    #[test]
    fn smc_repatch_restamps_with_fresh_immediates() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        let idx = warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

        // Run the loop with the region live, then execute the guest self-patcher: its
        // stores hit watched code bytes, bump the decode generation, and kill the stamp.
        arm_loop(&mut interp, &mut bus_i, 3);
        arm_loop(&mut jit_cpu, &mut bus_j, 3);
        drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        let entries_before = jit_cpu.perf_counters().jit_region_entries;
        assert!(entries_before > 0);
        for (cpu, bus) in [(&mut interp, &mut bus_i), (&mut jit_cpu, &mut bus_j)] {
            cpu.registers.eip = PATCHER;
            drive_to_halt(cpu, bus, u64::MAX);
        }
        assert_eq!(bus_j.memory[0x104..0x108], PATCHED_IMM.to_le_bytes());

        // Re-warm interpreted (the dead line means no region runs), then re-admit: the
        // matcher must find the SAME region and refresh its slot table wholesale, patched
        // immediates riding along in the fresh decodes.
        arm_loop(&mut interp, &mut bus_i, 2);
        arm_loop(&mut jit_cpu, &mut bus_j, 2);
        drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        assert_eq!(
            jit_cpu.perf_counters().jit_region_entries,
            entries_before,
            "a dead stamp must keep the region cold until re-admission"
        );
        let idx2 = jit::block::try_admit(&mut jit_cpu, ENTRY, true)
            .expect("the re-warmed block still builds");
        assert_eq!(idx2, idx, "re-admission must reuse the installed region");
        jit_cpu.decode_cache.stamp_region(ENTRY, true, idx2);
        {
            let region = jit_cpu.jit_regions.get_mut(idx2).unwrap();
            assert_eq!(region.ctx.slots[1].insn.imm, PATCHED_IMM);
            assert_eq!(region.ctx.slots[5].insn.imm, PATCHED_IMM);
        }

        arm_loop(&mut interp, &mut bus_i, 6);
        arm_loop(&mut jit_cpu, &mut bus_j, 6);
        let calls_i = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let calls_j = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        assert_eq!(calls_i, calls_j);
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        assert!(jit_cpu.perf_counters().jit_region_entries > entries_before);
    }

    #[test]
    fn profiling_falls_back_to_the_interpreter() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);

        jit_cpu.profile.enable(1_000_000);
        arm_loop(&mut interp, &mut bus_i, 4);
        arm_loop(&mut jit_cpu, &mut bus_j, 4);
        drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);

        assert_eq!(
            jit_cpu.perf_counters().jit_region_entries,
            0,
            "profiled runs must not enter the region (per-instruction sampling)"
        );
        assert_eq!(interp.registers, jit_cpu.registers);
        assert_eq!(bus_i.memory, bus_j.memory);
    }

    /// A `TestBus` wrapper adding the two machine-bus behaviors this port-free loop can never
    /// raise on `TestBus` itself: `requires_step_break` arms on the Nth memory write
    /// (standing in for the `io_touched` edge; the driver clears it per run like the machine
    /// batch loop), and `in_batch_scaled_bus_clocks` reports a synthetic monotonic count (2
    /// per bus access) so the run cap's bus-growth term is live, as it is at 486/586 on the
    /// real machine bus.
    struct InstrumentedBus {
        inner: TestBus,
        writes_until_break: u32,
        armed: bool,
        bus_clocks: u64,
    }

    impl InstrumentedBus {
        fn new(memory: Vec<u8>) -> Self {
            Self {
                inner: TestBus::with_memory(memory),
                writes_until_break: u32::MAX, // step break disarmed
                armed: false,
                bus_clocks: 0,
            }
        }
    }

    impl CpuBus for InstrumentedBus {
        fn read_memory(
            &mut self,
            address: u32,
            width: BusWidth,
            kind: BusAccessKind,
        ) -> Result<u32, BusError> {
            self.bus_clocks += 2;
            self.inner.read_memory(address, width, kind)
        }
        fn write_memory(
            &mut self,
            address: u32,
            width: BusWidth,
            value: u32,
            kind: BusAccessKind,
        ) -> Result<(), BusError> {
            self.bus_clocks += 2;
            if self.writes_until_break > 0 {
                self.writes_until_break -= 1;
                if self.writes_until_break == 0 {
                    self.armed = true;
                }
            }
            self.inner.write_memory(address, width, value, kind)
        }
        fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
            self.inner.prefetch_memory(address, out)
        }
        fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
            self.bus_clocks += 2;
            self.inner.charge_instruction_fetch(address)
        }
        fn in_batch_scaled_bus_clocks(&self) -> u64 {
            self.bus_clocks
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
            cpu_is_ring0_pm: bool,
        ) -> Result<(), BusError> {
            self.inner.write_io(port, width, value, cpu_is_ring0_pm)
        }
        fn interrupt_acknowledge(&mut self, vector: u8, ax: u16) -> Result<(), BusError> {
            self.inner.interrupt_acknowledge(vector, ax)
        }
        fn requires_step_break(&self) -> bool {
            self.armed || self.inner.requires_step_break()
        }
    }

    #[test]
    fn region_breaks_at_the_step_break_boundary() {
        // Arm the break on the 5th guest write: mid-iteration-2, on the region's slot-6
        // store. Both executions must end that run at exactly that instruction boundary.
        let run = |admit: bool| {
            let mut cpu = fresh_cpu(0xffff);
            let mut bus = InstrumentedBus::new(program());
            arm_loop(&mut cpu, &mut bus.inner, 2);
            for _ in 0..1000 {
                if cpu.run_straight_line(&mut bus, u64::MAX).unwrap().halted {
                    break;
                }
            }
            if admit {
                let idx = jit::block::try_admit(&mut cpu, ENTRY, true).unwrap();
                cpu.decode_cache.stamp_region(ENTRY, true, idx);
            }
            arm_loop(&mut cpu, &mut bus.inner, 6);
            bus.writes_until_break = 5;
            let mut boundaries = Vec::new();
            for _ in 0..1000 {
                bus.armed = false; // the machine batch loop clears io_touched per batch
                let outcome = cpu.run_straight_line(&mut bus, u64::MAX).unwrap();
                boundaries.push((outcome.core_clocks, cpu.registers.eip));
                if outcome.halted {
                    break;
                }
            }
            (boundaries, cpu, bus)
        };
        let (bounds_i, cpu_i, bus_i) = run(false);
        let (bounds_j, cpu_j, bus_j) = run(true);
        assert_eq!(bounds_i, bounds_j, "step-break boundaries diverged");
        assert_eq!(cpu_i, cpu_j);
        assert_eq!(bus_i.inner.memory, bus_j.inner.memory);
        assert_eq!(bus_i.inner.trace.cycles(), bus_j.inner.trace.cycles());
        assert!(cpu_j.perf_counters().jit_region_entries > 0);
    }

    #[test]
    fn cap_bus_growth_term_breaks_identically_on_a_clock_reporting_bus() {
        // The run cap check adds the bus's in-batch scaled clock GROWTH to the core total
        // (nonzero on the real 486/586 machine bus). With the synthetic 2-clocks-per-access
        // counter live, the region's per-slot cap check must break at exactly the
        // interpreter's instruction boundary.
        let run = |admit: bool, cap: u64| {
            let mut cpu = fresh_cpu(0xffff);
            let mut bus = InstrumentedBus::new(program());
            arm_loop(&mut cpu, &mut bus.inner, 2);
            for _ in 0..1000 {
                if cpu.run_straight_line(&mut bus, u64::MAX).unwrap().halted {
                    break;
                }
            }
            if admit {
                let idx = jit::block::try_admit(&mut cpu, ENTRY, true).unwrap();
                cpu.decode_cache.stamp_region(ENTRY, true, idx);
            }
            arm_loop(&mut cpu, &mut bus.inner, 10);
            let mut boundaries = Vec::new();
            for _ in 0..10_000 {
                let outcome = cpu.run_straight_line(&mut bus, cap).unwrap();
                boundaries.push((outcome.core_clocks, cpu.registers.eip, bus.bus_clocks));
                if outcome.halted {
                    break;
                }
            }
            (boundaries, cpu, bus)
        };
        for cap in [60u64, 145, 400] {
            let (bounds_i, cpu_i, bus_i) = run(false, cap);
            let (bounds_j, cpu_j, bus_j) = run(true, cap);
            assert_eq!(
                bounds_i, bounds_j,
                "cap {cap}: bus-growth boundaries diverged"
            );
            assert_eq!(cpu_i, cpu_j);
            assert_eq!(bus_i.inner.trace.cycles(), bus_j.inner.trace.cycles());
            assert!(cpu_j.perf_counters().jit_region_entries > 0);
        }
    }

    #[test]
    fn narrow_smc_kills_only_the_covering_lines() {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        assert!(cpu.decode_cache.line_live(ENTRY, true));
        assert!(cpu.decode_cache.line_live(0x102, true));
        let inval_before = cpu.perf_counters().decode_inval_smc;

        // The guest self-patcher writes the two imm32s at 0x104/0x111: covering lines
        // (0x102, 0x10f) die individually; every other loop line survives, and no
        // whole-cache flush happens.
        cpu.registers.eip = PATCHER;
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);

        assert_eq!(cpu.perf_counters().decode_inval_smc, inval_before);
        assert!(cpu.perf_counters().smc_narrow_kills >= 2);
        assert!(
            !cpu.decode_cache.line_live(0x102, true),
            "covering line must die"
        );
        assert!(
            !cpu.decode_cache.line_live(0x10f, true),
            "covering line must die"
        );
        assert!(
            cpu.decode_cache.line_live(ENTRY, true),
            "neighbor must survive"
        );
        assert!(
            cpu.decode_cache.line_live(0x108, true),
            "neighbor must survive"
        );
        assert!(
            cpu.decode_cache.line_live(0x131, true),
            "neighbor must survive"
        );
    }

    #[test]
    fn narrow_smc_falls_back_globally_on_an_aliased_page() {
        // Two linear pages decoding through the same physical page make the
        // physical-to-linear reconstruction ambiguous: narrow_invalidate must refuse.
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let insn = cpu.decode_cache.get(ENTRY, true).unwrap();
        // A second mapping: linear 0x5100 claims the same physical 0x100.
        cpu.decode_cache.put(0x5100, insn, true, ENTRY);
        assert!(
            cpu.decode_cache.narrow_invalidate(ENTRY).is_none(),
            "an aliased physical page must force the global flush"
        );
    }

    #[test]
    fn narrow_smc_falls_back_globally_on_a_straddling_instruction() {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let insn = cpu.decode_cache.get(0x102, true).unwrap(); // 6-byte add
        // Pretend it was decoded straddling the page edge at 0xffe: both pages flag.
        cpu.decode_cache.put(0xffe, insn, true, 0xffe);
        assert!(cpu.decode_cache.narrow_invalidate(0xffe).is_none());
        assert!(cpu.decode_cache.narrow_invalidate(0x1001).is_none());
    }

    #[test]
    fn builder_admits_a_different_but_valid_loop_shape() {
        // Same program with the first SHR count byte changed (0x19 -> 0x18): a different but
        // still valid continuable self-loop. The old matcher pinned the exact drawcolumn shape
        // and rejected this; the generic builder admits ANY continuable basic block, so it now
        // compiles it as a 15-slot self-loop (the point of the generalization).
        let mut memory = program();
        memory[0x10c] = 0x18;
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(memory);
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let idx = jit::block::try_admit(&mut cpu, ENTRY, true)
            .expect("a valid continuable self-loop must build");
        let region = cpu.jit_regions.get_mut(idx).unwrap();
        assert_eq!(
            region.ctx.slots.len(),
            15,
            "same shape, different shift count"
        );
        assert!(region.is_loop, "the back-edge still targets the entry");
        assert_eq!(
            region.ctx.slots[3].insn.imm, 24,
            "the mutated shift count rode along into the slot"
        );
    }

    #[test]
    fn cold_decode_lines_defer_admission() {
        // Before any execution the decode cache is empty: admission must return None
        // rather than reading guest memory itself.
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        arm_loop(&mut cpu, &mut bus, 1);
        assert!(jit::block::try_admit(&mut cpu, ENTRY, true).is_none());
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        assert!(jit::block::try_admit(&mut cpu, ENTRY, true).is_some());
    }

    /// S2.2 end-to-end prototype (owner chose "prototype first"): wire the native-bookkeeping
    /// path (`emit_native_bookkeeping`, native arithmetic replacing the `region_inline_slot`
    /// CALL for inline slots), VERIFY it is bit-identical on the real drawcolumn, then time it
    /// A/B vs the CALL path through the real `run_straight_line`. The micro-benchmarks were
    /// confounded; this is the honest measurement.
    ///   cargo test -j8 -p izarravm-cpu --release --features jit s2_native_bookkeeping -- --ignored --nocapture
    #[test]
    #[ignore]
    fn s2_native_bookkeeping_prototype() {
        use std::sync::atomic::Ordering;
        use std::time::Instant;

        // Part 1: BIT-IDENTITY on the drawcolumn (native-bookkeeping region vs interpreter).
        jit::block::NATIVE_BOOKKEEPING.store(1, Ordering::Relaxed);
        {
            let mut interp = fresh_cpu(0xffff);
            let mut jit_cpu = fresh_cpu(0xffff);
            let mut bus_i = TestBus::with_memory(program());
            let mut bus_j = TestBus::with_memory(program());
            warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
            arm_loop(&mut interp, &mut bus_i, 8);
            arm_loop(&mut jit_cpu, &mut bus_j, 8);
            let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
            let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
            assert_eq!(ci, cj, "native bookkeeping: per-run outcomes diverged");
            assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
            assert!(jit_cpu.perf_counters().jit_region_entries > 0);
        }
        jit::block::NATIVE_BOOKKEEPING.store(0, Ordering::Relaxed);
        eprintln!("native bookkeeping BIT-IDENTICAL on the drawcolumn (region vs interpreter)");

        // Part 2: timed A/B through run_straight_line. Big memory so edi can advance across many
        // iterations; tracing off (representative of production, not the traced test bus).
        const ITERS: u32 = 200_000;
        let time_variant = |mode: u8| -> f64 {
            jit::block::NATIVE_BOOKKEEPING.store(mode, Ordering::Relaxed);
            let mut m = vec![0u8; 64 << 20];
            let p = program();
            m[..p.len()].copy_from_slice(&p);
            let mut cpu = fresh_cpu(0xffff_ffff);
            let mut bus = TestBus::with_memory(m);
            bus.direct_pages_enabled = true; // host-pointer cache path, like production
            bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
            arm_loop(&mut cpu, &mut bus, 2);
            drive_to_halt(&mut cpu, &mut bus, u64::MAX);
            let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
            cpu.decode_cache.stamp_region(ENTRY, true, idx);
            let mut best = f64::MAX;
            for _ in 0..7 {
                arm_loop(&mut cpu, &mut bus, ITERS);
                let t = Instant::now();
                drive_to_halt(&mut cpu, &mut bus, u64::MAX);
                best = best.min(t.elapsed().as_secs_f64() / (ITERS as f64 * 15.0) * 1e9);
            }
            best
        };
        let call = time_variant(0);
        let native = time_variant(1);
        jit::block::NATIVE_BOOKKEEPING.store(0, Ordering::Relaxed);
        eprintln!("\n=== S2.2 native-bookkeeping A/B (drawcolumn, ns/insn, best of 7) ===");
        eprintln!("0. call bookkeeping (today's region) : {call:.3} ns/insn");
        eprintln!(
            "1. native bookkeeping (emit_native)  : {native:.3} ns/insn  ({:.2}x)",
            call / native
        );
        eprintln!("   -> the inline bookkeeping CALL is ~2 ns/slot x 8 inline slots ~= 16 ns of a");
        eprintln!(
            "      ~{:.0} ns iteration (~1%); even a fully-native version cannot move the",
            call * 15.0
        );
        eprintln!(
            "      drawcolumn. Its cost is the 7 memory slots' execute dispatch (the S2.4 lever)."
        );
        eprintln!("=== end A/B ===\n");
    }

    /// The region trampoline must stay byte-identical to the interpreter on the host-pointer
    /// direct-page path too: with `direct_pages_enabled` the bus hands out host pages, so data
    /// accesses are cached derefs (`data_read_pages`/`data_write_pages`) rather than the slow
    /// `read_memory_direct` fallback the rest of the differential suite exercises. This is the
    /// production-representative memory path (MachineBus always hands out direct pages).
    #[test]
    fn region_is_byte_identical_on_the_direct_page_path() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());
        bus_i.direct_pages_enabled = true;
        bus_j.direct_pages_enabled = true;
        warm_and_admit(&mut interp, &mut bus_i, &mut jit_cpu, &mut bus_j);
        arm_loop(&mut interp, &mut bus_i, 8);
        arm_loop(&mut jit_cpu, &mut bus_j, 8);
        let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        assert_eq!(ci, cj, "direct-page path: per-run outcomes diverged");
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
        assert!(
            jit_cpu.perf_counters().direct_page_hits > 0,
            "the direct-page (host-pointer) path was exercised"
        );
    }

    /// Baseline drawcolumn region throughput on a production-representative harness (the one-op
    /// instruction-fetch charge and host-pointer direct pages, both matching MachineBus). The
    /// reference the eventual native-template build's A/B measures against. The full cost
    /// decomposition, and why the incremental fast paths (S2.2 bookkeeping, S2.4 memory) are not
    /// the lever, are in dev_docs/2026-07-08-s2.4-memory-fast-path-results.md: on this harness
    /// the drawcolumn is ~165 ns/iter with a flat cost distribution (no single 50% lever), so
    /// only a full native-template dynarec reaches the 6.7x target.
    ///   cargo test -j8 -p izarravm-cpu --release --features jit drawcolumn_region_baseline -- --ignored --nocapture
    #[test]
    #[ignore]
    fn drawcolumn_region_baseline() {
        use std::time::Instant;
        const ITERS: u32 = 200_000;
        let mut m = vec![0u8; 64 << 20];
        let p = program();
        m[..p.len()].copy_from_slice(&p);
        let mut cpu = fresh_cpu(0xffff_ffff);
        let mut bus = TestBus::with_memory(m);
        bus.direct_pages_enabled = true; // host-pointer cache path, like production
        bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
        cpu.decode_cache.stamp_region(ENTRY, true, idx);
        let mut best = f64::MAX;
        for _ in 0..7 {
            arm_loop(&mut cpu, &mut bus, ITERS);
            let t = Instant::now();
            drive_to_halt(&mut cpu, &mut bus, u64::MAX);
            best = best.min(t.elapsed().as_secs_f64() / ITERS as f64 * 1e9);
        }
        eprintln!(
            "drawcolumn region baseline: {best:.0} ns/iter (15 insns), representative harness"
        );
    }

    /// Cost-fold native-LOAD smoke A/B on the drawcolumn (flat DS, unpaged, direct pages — the only
    /// mode where the fold fires; the anchors run PAGED so the fold is inert there and Doom is the
    /// real gate, per dev_docs). Times the same drawcolumn with the fold OFF then ON. RUN FILTERED
    /// (this sets the process-global FOLD_TIMING; a concurrent flat-DS #[ignore] bench would see it):
    ///   cargo test -p izarravm-cpu --release --features jit drawcolumn_region_fold_ab -- --ignored --nocapture
    #[test]
    #[ignore]
    fn drawcolumn_region_fold_ab() {
        use std::sync::atomic::Ordering;
        use std::time::Instant;
        const ITERS: u32 = 200_000;
        let time_variant = |fold: bool| -> (f64, u64, u64) {
            jit::block::FOLD_TIMING.store(fold, Ordering::Relaxed);
            let mut m = vec![0u8; 64 << 20];
            let p = program();
            m[..p.len()].copy_from_slice(&p);
            let mut cpu = fresh_cpu(0xffff_ffff);
            let mut bus = TestBus::with_memory(m);
            bus.direct_pages_enabled = true;
            bus.trace.set_tracing_mode(izarravm_bus::TracingMode::Off);
            arm_loop(&mut cpu, &mut bus, 2);
            drive_to_halt(&mut cpu, &mut bus, u64::MAX);
            let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
            cpu.decode_cache.stamp_region(ENTRY, true, idx);
            let mut best = f64::MAX;
            for _ in 0..7 {
                arm_loop(&mut cpu, &mut bus, ITERS);
                let t = Instant::now();
                drive_to_halt(&mut cpu, &mut bus, u64::MAX);
                best = best.min(t.elapsed().as_secs_f64() / ITERS as f64 * 1e9);
            }
            let pc = cpu.perf_counters();
            (best, pc.jit_native_load_hits, pc.jit_native_store_hits)
        };
        let (off, off_ld, off_st) = time_variant(false);
        let (on, on_ld, on_st) = time_variant(true);
        jit::block::FOLD_TIMING.store(false, Ordering::Relaxed);
        eprintln!("\n=== drawcolumn cost-fold native LOAD+STORE+ALU A/B (ns/iter, best of 7) ===");
        eprintln!("fold OFF: {off:.0} ns/iter  (load_hits={off_ld}, store_hits={off_st})");
        eprintln!(
            "fold ON : {on:.0} ns/iter  (load_hits={on_ld}, store_hits={on_st})  ({:.2}x)",
            off / on
        );
        assert_eq!(
            (off_ld, off_st),
            (0, 0),
            "fold-off must run no native slots"
        );
        assert!(on_ld > 0, "fold-on must run the native LOAD slots");
        assert!(on_st > 0, "fold-on must run the native STORE slots");
        eprintln!("=== end A/B (the anchors run PAGED, so this fold is Doom-inert) ===\n");
    }

    /// Round 1 hotness admission: with `set_jit_auto_admit(true)` and NO manual `try_admit`, a
    /// hot loop compiles itself once its entry line crosses JIT_HOTNESS_THRESHOLD, and the
    /// auto-admitted region stays byte-identical to the interpreter. The interp CPU (auto-admit
    /// off) never compiles, proving the flag gates it.
    #[test]
    fn hotness_admission_compiles_a_hot_loop_and_stays_identical() {
        let mut interp = fresh_cpu(0xffff);
        let mut jit_cpu = fresh_cpu(0xffff);
        jit_cpu.set_jit_auto_admit(true);
        // A 64 KB buffer holds the ~0x2D00 that edi reaches over 64 iterations (0x500 + 64*0xa0).
        let mut bus_i = TestBus::with_memory(program_in(0x1_0000));
        let mut bus_j = TestBus::with_memory(program_in(0x1_0000));
        // 64 iterations: past the threshold (32), so the loop auto-admits mid-run and the region
        // runs the remaining iterations.
        arm_loop(&mut interp, &mut bus_i, 64);
        arm_loop(&mut jit_cpu, &mut bus_j, 64);
        let ci = drive_to_halt(&mut interp, &mut bus_i, u64::MAX);
        let cj = drive_to_halt(&mut jit_cpu, &mut bus_j, u64::MAX);
        assert_eq!(ci, cj, "hotness admission: per-run outcomes diverged");
        assert_identical(&interp, &bus_i, &jit_cpu, &bus_j);
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "the hot loop auto-admitted and ran a region"
        );
        assert_eq!(
            interp.perf_counters().jit_region_entries,
            0,
            "auto-admit off: the interpreter never compiles"
        );
    }

    /// Auto-admit stays OFF by default: the same hot loop, without `set_jit_auto_admit`, never
    /// compiles (so existing manual-admission tests and default runs are undisturbed).
    #[test]
    fn no_auto_admit_by_default() {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program_in(0x1_0000));
        arm_loop(&mut cpu, &mut bus, 64);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        assert_eq!(
            cpu.perf_counters().jit_region_entries,
            0,
            "no region should compile without auto-admit or the forced address"
        );
    }

    /// The capacity-GC primitive: `RegionTable::clear` + a decode-generation bump must leave NO
    /// live stamp pointing into the emptied table, so `try_admit`'s clear-on-full can never
    /// follow a dangling index. Admit a region, confirm it resolves, clear + invalidate, confirm
    /// the stamp no longer resolves.
    #[test]
    fn clear_and_invalidate_drops_region_stamps() {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        let idx = jit::block::try_admit(&mut cpu, ENTRY, true).expect("admit");
        cpu.decode_cache.stamp_region(ENTRY, true, idx);
        assert_eq!(cpu.decode_cache.region_at(ENTRY, true), Some(idx));
        cpu.jit_regions.clear();
        cpu.decode_cache.invalidate();
        assert_eq!(
            cpu.decode_cache.region_at(ENTRY, true),
            None,
            "a cleared table must leave no resolvable stamp"
        );
        assert_eq!(cpu.jit_regions.len(), 0);
    }

    /// `run_region` unstamps a stale region (SMC epoch / mode-key mismatch) while leaving the
    /// entry line LIVE - no generation bump, no re-decode - so its hotness counter is NOT reset
    /// by `put`. Without `unstamp_region` re-priming it, the fire-once counter stays pinned at
    /// the threshold and, under pure auto-admit (no forced address to re-trigger `try_admit`),
    /// the loop de-JITs permanently. This tests the primitive directly: an unstamp of a live,
    /// hot line must leave it ready to re-fire admission on the very next miss. (An integration
    /// test cannot reliably reach this state - the drawcolumn self-patcher and a segment reload
    /// both bump the decode generation, which re-decodes the entry line and resets hotness via
    /// `put`, masking the gap.)
    #[test]
    fn unstamp_reprimes_hotness_so_a_stale_region_re_admits() {
        let mut cpu = fresh_cpu(0xffff);
        let mut bus = TestBus::with_memory(program());
        // Warm the ENTRY line so its decode is live (auto-admit off, so hotness stays 0).
        arm_loop(&mut cpu, &mut bus, 2);
        drive_to_halt(&mut cpu, &mut bus, u64::MAX);
        // Drive the counter across the threshold (fires once), then confirm it is pinned.
        let mut fired = false;
        for _ in 0..64 {
            fired |= cpu.decode_cache.note_hot_miss(ENTRY, true);
        }
        assert!(
            fired,
            "hotness crosses the threshold and fires admission once"
        );
        assert!(
            !cpu.decode_cache.note_hot_miss(ENTRY, true),
            "the fire-once counter is pinned after firing"
        );
        // Unstamping a live line (run_region's stale-region path) must re-prime it, so the very
        // next miss re-fires. Without the fix this stays false and the loop never re-admits.
        cpu.decode_cache.unstamp_region(ENTRY, true);
        assert!(
            cpu.decode_cache.note_hot_miss(ENTRY, true),
            "unstamp re-primes hotness so the next miss re-fires admission"
        );
    }
}

/// DYN-S1: the generic block builder. These exercise coverage the single drawcolumn shape did
/// not: an x87-containing loop (the four-accumulator identity, incl. `fp_rem`), a self-loop
/// livelock guard, a LINEAR (non-loop) block, and the behavioral terminator predicate.
#[cfg(feature = "jit")]
mod jit_general {
    use super::*;

    /// Real mode with a 32-bit code segment (flat, 64 KB limit), at the 586 level so the FP
    /// timing classes are non-identity and `fp_rem` actually carries.
    fn fresh() -> Cpu386 {
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I586);
        cpu.load_segment_real(SegmentIndex::Cs, 0);
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        let mut cs = cpu.registers.cs();
        cs.default_size_32 = true;
        cpu.registers.set_segment(SegmentIndex::Cs, cs);
        cpu
    }

    fn drive_to_halt(cpu: &mut Cpu386, bus: &mut TestBus) {
        for _ in 0..10_000 {
            if cpu.run_straight_line(bus, u64::MAX).unwrap().halted {
                return;
            }
        }
        panic!("guest never halted");
    }

    // ---- 1. Four-accumulator identity on an x87-containing loop ----

    const X87_START: u32 = 0x100;
    const X87_LOOP: u32 = 0x101;
    const X87_COUNT: usize = 0x400;

    /// NOP starter, then a self-loop mixing an ALU op, a memory store, two x87 memory ops (the
    /// IntConvert32 class, x34 at 586, so `fp_rem` carries hard) and an FNINIT (Register class,
    /// x0.25) so the block spans two FP classes, a memory-counter DEC, and the rel8 back-edge.
    /// FNINIT each iteration keeps the x87 stack balanced regardless of the reset FPU state.
    fn x87_program() -> Vec<u8> {
        let mut m = vec![0u8; 0x1000];
        m[X87_START as usize] = 0x90; // nop starter, so X87_LOOP is reached as a continuation
        let body: [u8; 18] = [
            0xdb, 0xe3, // fninit                 (Register class)
            0xdb, 0x06, // fild dword [esi]       (IntConvert32)
            0xdb, 0x1f, // fistp dword [edi]      (IntConvert32)
            0x83, 0xc3, 0x03, // add ebx,3        (ALU)
            0x89, 0x5f, 0x04, // mov [edi+4],ebx  (memory store)
            0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [X87_COUNT] (memory RMW)
        ];
        m[X87_LOOP as usize..X87_LOOP as usize + body.len()].copy_from_slice(&body);
        let jnz_at = X87_LOOP as usize + body.len();
        let rel = (X87_LOOP as i32 - (jnz_at as i32 + 2)) as i8;
        m[jnz_at] = 0x75; // jnz X87_LOOP
        m[jnz_at + 1] = rel as u8;
        m[jnz_at + 2] = 0xf4; // hlt at the loop fall-through
        m[0x300..0x304].copy_from_slice(&1234u32.to_le_bytes()); // the int fild reads
        m
    }

    fn x87_arm(cpu: &mut Cpu386, bus: &mut TestBus, count: u32) {
        cpu.registers.eip = X87_START;
        cpu.registers.set_esp(0x0700);
        cpu.registers.set_eax(0);
        cpu.registers.set_ebx(0);
        cpu.registers.set_esi(0x300);
        cpu.registers.set_edi(0x310);
        bus.memory[X87_COUNT..X87_COUNT + 4].copy_from_slice(&count.to_le_bytes());
    }

    fn count_of(bus: &TestBus) -> u32 {
        u32::from_le_bytes(bus.memory[X87_COUNT..X87_COUNT + 4].try_into().unwrap())
    }

    /// Drive until the memory counter hits zero (the loop fall-through), which is BEFORE the
    /// trailing HLT is executed as a fresh run's first instruction (that would reset
    /// `core_clocks_so_far`). This is the point at which the four accumulators are meaningfully
    /// compared.
    fn drive_until_count_zero(cpu: &mut Cpu386, bus: &mut TestBus) {
        for _ in 0..10_000 {
            let out = cpu.run_straight_line(bus, u64::MAX).unwrap();
            if count_of(bus) == 0 || out.halted {
                return;
            }
        }
        panic!("counter never reached zero");
    }

    #[test]
    fn general_block_four_accumulator_identity() {
        let mut interp = fresh();
        let mut jit_cpu = fresh();
        let mut bus_i = TestBus::with_memory(x87_program());
        let mut bus_j = TestBus::with_memory(x87_program());

        // Warm both identically (fills the decode cache), then admit the loop on the jit CPU.
        x87_arm(&mut interp, &mut bus_i, 2);
        x87_arm(&mut jit_cpu, &mut bus_j, 2);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        assert_eq!(interp, jit_cpu, "warm phases must match before admission");
        assert_eq!(interp.fp_rem, jit_cpu.fp_rem, "warm fp_rem must match");

        let idx =
            jit::block::try_admit(&mut jit_cpu, X87_LOOP, true).expect("the x87 loop must build");
        {
            let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
            assert!(region.is_loop, "the x87 block is a self-loop");
            assert_eq!(
                region.ctx.slots.len(),
                7,
                "fninit+fild+fistp+add+mov+dec+jnz"
            );
        }
        jit_cpu.decode_cache.stamp_region(X87_LOOP, true, idx);

        // Measured run: eight iterations, driven to the loop fall-through.
        x87_arm(&mut interp, &mut bus_i, 8);
        x87_arm(&mut jit_cpu, &mut bus_j, 8);
        drive_until_count_zero(&mut interp, &mut bus_i);
        drive_until_count_zero(&mut jit_cpu, &mut bus_j);

        // THE gate: all four accumulators byte-identical, region vs interpreter.
        assert_eq!(
            interp.elapsed_clocks, jit_cpu.elapsed_clocks,
            "elapsed_clocks diverged"
        );
        assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem diverged");
        assert_eq!(
            interp.fp_rem, jit_cpu.fp_rem,
            "fp_rem diverged (x87 batching)"
        );
        assert_eq!(
            interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
            "core_clocks_so_far diverged"
        );
        // And the full architectural state + guest memory.
        assert_eq!(interp, jit_cpu, "architectural state diverged");
        assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");

        // The test is only meaningful if the region actually ran and the FP path carried a
        // remainder (a non-identity FP class was exercised).
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "the region never executed"
        );
        assert_eq!(interp.perf_counters().jit_region_entries, 0);
        assert!(
            interp.fp_rem != 0,
            "the FP timing remainder must be exercised (else the fp_rem check is vacuous)"
        );
    }

    // ---- 2. Self-loop livelock guard (jmp $) ----

    #[test]
    fn self_loop_advances_the_clock_and_stops_at_the_cap() {
        let mut cpu = fresh();
        let mut mem = vec![0u8; 0x1000];
        mem[0x100] = 0x90; // nop starter
        mem[0x101] = 0xeb; // jmp $ (rel8 -2 -> 0x101)
        mem[0x102] = 0xfe;
        let mut bus = TestBus::with_memory(mem);

        // Warm 0x100 and 0x101 (jmp $ never halts, so warm with a bounded finite-cap drive).
        cpu.registers.eip = 0x100;
        for _ in 0..8 {
            let _ = cpu.run_straight_line(&mut bus, 50);
        }

        let idx = jit::block::try_admit(&mut cpu, 0x101, true)
            .expect("jmp $ must build a 1-slot self-loop");
        {
            let region = cpu.jit_regions.get_mut(idx).unwrap();
            assert!(region.is_loop);
            assert_eq!(region.ctx.slots.len(), 1);
        }
        cpu.decode_cache.stamp_region(0x101, true, idx);

        cpu.registers.eip = 0x101;
        let before = cpu.elapsed_clocks;
        let entries_before = cpu.perf_counters().jit_region_entries;
        let out = cpu.run_straight_line(&mut bus, 1000).unwrap();

        assert!(!out.halted, "jmp $ never halts");
        assert!(
            cpu.elapsed_clocks > before,
            "the self-loop must advance the clock (no net-zero livelock)"
        );
        assert!(
            cpu.perf_counters().jit_region_entries > entries_before,
            "the region must have run"
        );
        assert_eq!(cpu.registers.eip, 0x101, "still looping at jmp $");
    }

    // ---- 3. A linear (non-loop) block runs identically to the interpreter ----

    const LIN_START: u32 = 0x100;
    const LIN_BODY: u32 = 0x101;

    fn linear_program() -> Vec<u8> {
        let mut m = vec![0u8; 0x1000];
        m[LIN_START as usize] = 0x90; // nop starter -> LIN_BODY is a continuation
        let body: [u8; 11] = [
            0x83, 0xc0, 0x05, // add eax,5
            0x83, 0xc3, 0x07, // add ebx,7
            0x89, 0x07, // mov [edi],eax
            0x83, 0xc1, 0x01, // add ecx,1
        ];
        m[LIN_BODY as usize..LIN_BODY as usize + body.len()].copy_from_slice(&body);
        m[LIN_BODY as usize + body.len()] = 0xf4; // hlt terminates the block
        m
    }

    fn lin_arm(cpu: &mut Cpu386) {
        cpu.registers.eip = LIN_START;
        cpu.registers.set_esp(0x0700);
        cpu.registers.set_eax(0x1111);
        cpu.registers.set_ebx(0x2222);
        cpu.registers.set_ecx(0);
        cpu.registers.set_edi(0x310);
    }

    #[test]
    fn linear_block_matches_the_interpreter() {
        let mut interp = fresh();
        let mut jit_cpu = fresh();
        let mut bus_i = TestBus::with_memory(linear_program());
        let mut bus_j = TestBus::with_memory(linear_program());

        lin_arm(&mut interp);
        lin_arm(&mut jit_cpu);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        assert_eq!(interp, jit_cpu, "warm phases must match before admission");

        let idx = jit::block::try_admit(&mut jit_cpu, LIN_BODY, true)
            .expect("the linear block must build");
        {
            let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
            assert!(!region.is_loop, "a straight-line block is not a self-loop");
            assert_eq!(
                region.ctx.slots.len(),
                4,
                "add,add,mov,add (hlt is the terminator)"
            );
        }
        jit_cpu.decode_cache.stamp_region(LIN_BODY, true, idx);

        lin_arm(&mut interp);
        lin_arm(&mut jit_cpu);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);

        assert_eq!(
            interp.elapsed_clocks, jit_cpu.elapsed_clocks,
            "elapsed_clocks"
        );
        assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem");
        assert_eq!(interp, jit_cpu, "architectural state diverged");
        assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "the linear block region never ran"
        );
    }

    // ---- 4. The behavioral terminator predicate (§2.9) ----

    fn decode_one(bytes: &[u8]) -> DecodedInsn {
        let mut cpu = fresh();
        let mut mem = vec![0u8; 0x100];
        mem[..bytes.len()].copy_from_slice(bytes);
        let mut bus = TestBus::with_memory(mem);
        cpu.registers.eip = 0;
        cpu.decode(&mut bus).expect("opcode decodes")
    }

    #[test]
    fn terminator_predicate_covers_clock_device_and_interrupt_ops() {
        // Interior-eligible ops: fall through, no interrupt-visibility change, continuable.
        for (bytes, name) in [
            (&[0x83, 0xc1, 0x01][..], "add ecx,1"),
            (&[0x89, 0xd8][..], "mov eax,ebx"),
            (&[0x8e, 0xd8][..], "mov ds,ax (not SS)"),
            (
                &[0xec][..],
                "in al,dx (Approximate: runtime step-break, interior)",
            ),
            (&[0xd9, 0xe8][..], "fld1 (x87)"),
        ] {
            let insn = decode_one(bytes);
            assert!(
                jit::block::is_interior_eligible(&insn),
                "{name} must be an interior slot"
            );
        }

        // Hard terminators: not continuable at all (build_block stops before them).
        for (bytes, name) in [
            (&[0xf4][..], "hlt"),
            (&[0xee][..], "out dx,al"),
            (&[0xe6, 0x00][..], "out imm8,al"),
            (&[0x6c][..], "insb"),
            (&[0x6e][..], "outsb"),
            (&[0x0f, 0x31][..], "rdtsc (reads elapsed_clocks)"),
            (&[0x0f, 0x30][..], "wrmsr"),
            (&[0x0f, 0x22, 0xc0][..], "mov cr0,eax"),
            (&[0x0f, 0x01, 0x10][..], "lgdt [eax]"),
            (&[0xcd, 0x21][..], "int 21h"),
            (&[0xcf][..], "iret"),
        ] {
            let insn = decode_one(bytes);
            assert!(
                !insn.continuable,
                "{name} must be a non-continuable hard terminator"
            );
            assert!(
                !jit::block::is_interior_eligible(&insn),
                "{name} must not be an interior slot"
            );
        }

        // The load-bearing gap: IF/shadow changers are `continuable` (the interpreter runs
        // them inline with a per-instruction interrupt check) but MUST be excluded from
        // interior slots, because the region defers that check to the boundary.
        for (bytes, name) in [
            (&[0xfb][..], "sti"),
            (&[0xfa][..], "cli"),
            (&[0x9d][..], "popf"),
            (&[0x17][..], "pop ss"),
            (&[0x8e, 0xd0][..], "mov ss,ax"),
        ] {
            let insn = decode_one(bytes);
            assert!(
                insn.continuable,
                "{name} is continuable (the whole point of the gap)"
            );
            assert!(
                jit::block::changes_interrupt_visibility(&insn),
                "{name} must be flagged as an interrupt-visibility change"
            );
            assert!(
                !jit::block::is_interior_eligible(&insn),
                "{name} must not be an interior slot"
            );
        }

        // Control transfers are continuable but end the block as the terminal slot.
        for (bytes, name) in [
            (&[0xc3][..], "ret near"),
            (&[0x75, 0x00][..], "jnz rel8"),
            (&[0xeb, 0x00][..], "jmp rel8"),
        ] {
            let insn = decode_one(bytes);
            assert!(insn.continuable, "{name} is continuable");
            assert!(
                jit::block::is_control_transfer(&insn),
                "{name} must be flagged as a control transfer"
            );
            assert!(
                !jit::block::is_interior_eligible(&insn),
                "{name} must not be an interior slot"
            );
        }
    }

    // ---- 5. 16-bit register ops must not be inlined as 32-bit templates ----

    /// Real mode with a 16-bit code segment (the default DOS-game target): CS.D is clear, so
    /// the unprefixed mov/add/shr register forms are 16-bit ops.
    fn fresh16() -> Cpu386 {
        let mut cpu = Cpu386::default();
        cpu.set_level(CpuLevel::I586);
        cpu.load_segment_real(SegmentIndex::Cs, 0); // default_size_32 = false
        cpu.load_segment_real(SegmentIndex::Ds, 0);
        cpu.load_segment_real(SegmentIndex::Ss, 0);
        cpu.load_segment_real(SegmentIndex::Es, 0);
        cpu
    }

    #[test]
    fn sixteen_bit_register_ops_are_not_inlined_as_wrong_width() {
        // Regression for the operand-size gap: in a 16-bit segment the inline-able opcodes
        // (0x8B mov r,r; 0x81 /0 add r,imm; 0xC1 /5 shr r,imm) are 16-bit, so they must run
        // through the full trampoline step (correct width), NOT the 32-bit inline template
        // (which would clobber the upper 16 bits and compute 32-bit flags).
        let program = || {
            let mut m = vec![0u8; 0x1000];
            m[0x100] = 0x90; // nop starter, so 0x101 is reached as a continuation
            let body: [u8; 15] = [
                0x8b, 0xc3, // mov ax,bx           (16-bit: keeps EAX[31:16])
                0x81, 0xc0, 0x34, 0x12, // add ax,0x1234       (16-bit add + 16-bit flags)
                0xc1, 0xe8, 0x01, // shr ax,1            (16-bit shr)
                0xff, 0x0e, 0x00, 0x04, // dec word [0x400]    (16-bit memory RMW)
                0x75, 0x00, // jnz (rel patched below)
            ];
            m[0x101..0x101 + body.len()].copy_from_slice(&body);
            m[0x10f] = ((0x101i32 - 0x110i32) as i8) as u8; // jnz -> 0x101
            m[0x110] = 0xf4; // hlt
            m
        };
        let arm = |cpu: &mut Cpu386, bus: &mut TestBus, count: u16| {
            cpu.registers.eip = 0x100;
            cpu.registers.set_esp(0x0700);
            cpu.registers.set_eax(0xAAAA_0000); // distinct upper half
            cpu.registers.set_ebx(0xBBBB_2222);
            bus.memory[0x400..0x402].copy_from_slice(&count.to_le_bytes());
        };

        let mut interp = fresh16();
        let mut jit_cpu = fresh16();
        let mut bus_i = TestBus::with_memory(program());
        let mut bus_j = TestBus::with_memory(program());

        arm(&mut interp, &mut bus_i, 2);
        arm(&mut jit_cpu, &mut bus_j, 2);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        assert_eq!(interp, jit_cpu, "warm phases must match before admission");

        // d = false (16-bit segment).
        let idx =
            jit::block::try_admit(&mut jit_cpu, 0x101, false).expect("the 16-bit loop must build");
        {
            let region = jit_cpu.jit_regions.get_mut(idx).unwrap();
            assert!(region.is_loop);
            assert_eq!(region.ctx.slots.len(), 5);
            // The fix in the flesh: no 16-bit slot is an inline 32-bit template.
            for (i, s) in region.ctx.slots.iter().enumerate() {
                assert!(
                    matches!(
                        s.kind,
                        jit::step::SlotKind::Memory | jit::step::SlotKind::BackEdge
                    ),
                    "16-bit slot {i} must run through the full step, got {:?}",
                    s.kind
                );
            }
        }
        jit_cpu.decode_cache.stamp_region(0x101, false, idx);

        arm(&mut interp, &mut bus_i, 5);
        arm(&mut jit_cpu, &mut bus_j, 5);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);

        assert_eq!(
            interp, jit_cpu,
            "16-bit register ops diverged (wrong-width inline?)"
        );
        assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
        // The 16-bit ops must have preserved the upper half of EAX in both paths.
        assert_eq!(
            jit_cpu.registers.eax() & 0xFFFF_0000,
            0xAAAA_0000,
            "the JIT clobbered EAX[31:16] with a 32-bit op"
        );
        assert!(jit_cpu.perf_counters().jit_region_entries > 0);
    }

    // ---- S2.1: the per-op differential gate (template_diff) ----
    //
    // For each templated op, admit it as a single INTERIOR inline slot and run the region vs
    // the interpreter across flag-corner operands, asserting byte-identical guest state,
    // materialized eflags, all four accumulators, and guest memory. This is the gate every
    // native template must pass (a divergence in a width/wrap/undefined-flag corner fails
    // here); S2.3's templates each add a row. The op's flags must survive to the comparison,
    // so the loop back-edge is LOOP (0xE2), which decrements ECX and branches WITHOUT touching
    // the flags a `dec`/`jnz` counter would clobber.

    /// nop starter at 0x100, then `<op>` (the interior slot under test) at 0x101, then
    /// `loop 0x101` (the terminal back-edge, flag-neutral), then hlt at the fall-through.
    fn template_diff_program(op: &[u8]) -> Vec<u8> {
        let mut m = vec![0u8; 0x1000];
        m[0x100] = 0x90; // nop starter -> 0x101 is reached as a continuation
        let entry = 0x101usize;
        let mut p = entry;
        m[p..p + op.len()].copy_from_slice(op);
        p += op.len();
        let loop_at = p; // loop 0x101 (E2 rel8): ECX -= 1, branch if ECX != 0, sets NO flags
        m[p] = 0xe2;
        m[p + 1] = ((entry as i32) - (loop_at as i32 + 2)) as i8 as u8;
        p += 2;
        m[p] = 0xf4; // hlt
        m
    }

    /// Drive to the loop fall-through (ECX == 0), i.e. BEFORE the trailing HLT is executed as a
    /// fresh run's first instruction (which would reset core_clocks_so_far).
    fn drive_until_ecx_zero(cpu: &mut Cpu386, bus: &mut TestBus) {
        for _ in 0..10_000 {
            let out = cpu.run_straight_line(bus, u64::MAX).unwrap();
            if cpu.read_gpr32(1) == 0 || out.halted {
                return;
            }
        }
        panic!("ECX never reached zero");
    }

    /// Run one templated op through the region and the interpreter under `arm` (which sets the
    /// op's input registers, not ECX), and assert full identity. `expect_kind` pins that the
    /// op was actually inlined as the intended template (not a Memory fallback).
    fn assert_template_identity(
        op: &[u8],
        expect_kind: jit::step::SlotKind,
        arm: &dyn Fn(&mut Cpu386),
    ) {
        let entry = 0x101u32;
        let mut interp = fresh();
        let mut jit_cpu = fresh();
        let mut bus_i = TestBus::with_memory(template_diff_program(op));
        let mut bus_j = TestBus::with_memory(template_diff_program(op));

        let prep = |cpu: &mut Cpu386, ecx: u32| {
            cpu.registers.eip = 0x100;
            cpu.registers.set_esp(0x0700);
            cpu.registers.set_ecx(ecx); // LOOP counter (address-size 32 -> ECX)
            arm(cpu);
            // Seed a non-trivial incoming arithmetic-flag pattern so the PRESERVING templates
            // are tested against non-zero flags, not the default 0: a MOV that wrongly touched
            // any flag, or a SHR that clobbered its preserved AF or forced OF on a multi-bit
            // shift (both architecturally preserved / undefined), would otherwise be masked by
            // an all-zero incoming state. ZF is left clear so ZF-preservation is also observable.
            cpu.materialize_flags();
            const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
            let seed = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF | FLAG_OF;
            cpu.registers.eflags = (cpu.registers.eflags & !ARITH) | seed;
        };
        // Warm both (two iterations fill the decode cache), then admit on the jit CPU.
        prep(&mut interp, 2);
        prep(&mut jit_cpu, 2);
        drive_until_ecx_zero(&mut interp, &mut bus_i);
        drive_until_ecx_zero(&mut jit_cpu, &mut bus_j);
        let idx = jit::block::try_admit(&mut jit_cpu, entry, true)
            .unwrap_or_else(|| panic!("op {op:02x?} must build a self-loop"));
        assert_eq!(
            jit_cpu.jit_regions.get_mut(idx).unwrap().ctx.slots[0].kind,
            expect_kind,
            "op {op:02x?}: slot 0 must be the intended inline template"
        );
        jit_cpu.decode_cache.stamp_region(entry, true, idx);

        // Measured: one iteration under the swept operand.
        prep(&mut interp, 1);
        prep(&mut jit_cpu, 1);
        drive_until_ecx_zero(&mut interp, &mut bus_i);
        drive_until_ecx_zero(&mut jit_cpu, &mut bus_j);

        assert_eq!(interp, jit_cpu, "op {op:02x?}: guest state diverged");
        assert_eq!(
            interp.eflags(),
            jit_cpu.eflags(),
            "op {op:02x?}: materialized eflags diverged"
        );
        assert_eq!(
            interp.elapsed_clocks, jit_cpu.elapsed_clocks,
            "op {op:02x?}: elapsed_clocks diverged"
        );
        assert_eq!(
            interp.timing_rem, jit_cpu.timing_rem,
            "op {op:02x?}: timing_rem diverged"
        );
        assert_eq!(
            interp.fp_rem, jit_cpu.fp_rem,
            "op {op:02x?}: fp_rem diverged"
        );
        assert_eq!(
            interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
            "op {op:02x?}: core_clocks_so_far diverged"
        );
        assert_eq!(
            bus_i.memory, bus_j.memory,
            "op {op:02x?}: guest memory diverged"
        );
        assert!(
            jit_cpu.perf_counters().jit_region_entries > 0,
            "op {op:02x?}: region did not run"
        );
    }

    #[test]
    fn template_diff_add_r32_imm_across_flag_corners() {
        // add eax, imm32 (81 /0, the RegAddImm template). Sweep eax and imm across the carry
        // (0xffffffff+1), overflow (0x7fffffff+1, 0x80000000+0x80000000), sign, zero, and
        // parity corners; region and interpreter must agree on state, eflags, and all four
        // accumulators for every corner.
        let corners: [u32; 5] = [0, 1, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff];
        for &eax in &corners {
            for &imm in &corners {
                let mut op = vec![0x81u8, 0xc0]; // add eax, imm32
                op.extend_from_slice(&imm.to_le_bytes());
                assert_template_identity(
                    &op,
                    jit::step::SlotKind::RegAddImm { dst: 0, imm },
                    &|cpu: &mut Cpu386| cpu.registers.set_eax(eax),
                );
            }
        }
        // Register-addressing coverage: the emit addresses gpr[i] as [R14 + 4*i], so every
        // inline-eligible destination index must be exercised (skip ECX=1, the LOOP counter,
        // and ESP=4, the stack). A wrong displacement for a high index would pass an EAX-only
        // sweep.
        for &dst in &[0u8, 2, 3, 5, 6, 7] {
            let imm = 0x8000_0001u32; // carry + overflow + sign in one operand
            let mut op = vec![0x81u8, 0xc0 + dst]; // add <dst>, imm32
            op.extend_from_slice(&imm.to_le_bytes());
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegAddImm { dst, imm },
                &|cpu: &mut Cpu386| cpu.write_gpr32(dst, 0x8000_0001),
            );
        }
    }

    #[test]
    fn template_diff_shr_r32_imm_across_shift_corners() {
        // shr eax, imm8 (C1 /5, the RegShrImm template). Sweep the value and the count across
        // the CF-from-last-bit-out, OF (count 1), sign, zero, and parity corners.
        let vals: [u32; 6] = [0, 1, 0x8000_0001, 0xffff_ffff, 0x7fff_fffe, 0x0000_00ff];
        let counts: [u8; 5] = [1, 2, 7, 25, 31];
        for &eax in &vals {
            for &count in &counts {
                let op = vec![0xc1u8, 0xe8, count]; // shr eax, count
                assert_template_identity(
                    &op,
                    jit::step::SlotKind::RegShrImm { dst: 0, count },
                    &|cpu: &mut Cpu386| cpu.registers.set_eax(eax),
                );
            }
        }
        // Register-addressing coverage across all inline-eligible destinations (with the
        // incoming AF/OF seeded by `prep`, this also pins the preserved-flag path per index).
        for &dst in &[0u8, 2, 3, 5, 6, 7] {
            let count = 7u8; // multi-bit shift: OF falls back to live, AF is preserved
            let op = vec![0xc1u8, 0xe8 + dst, count]; // shr <dst>, count
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegShrImm { dst, count },
                &|cpu: &mut Cpu386| cpu.write_gpr32(dst, 0x8000_0001),
            );
        }
    }

    #[test]
    fn template_diff_mov_r32_r32_preserves_state() {
        // mov eax, ebx (8B /r, the RegMov template). No flags; sweep the source value and
        // confirm the destination is a faithful full-32-bit copy with flags untouched.
        let vals: [u32; 5] = [0, 0xdead_beef, 0xffff_ffff, 0x8000_0000, 0x1234_5678];
        for &ebx in &vals {
            let op = vec![0x8bu8, 0xc3]; // mov eax, ebx
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegMov { dst: 0, src: 3 },
                &|cpu: &mut Cpu386| {
                    cpu.registers.set_eax(0xaaaa_5555);
                    cpu.registers.set_ebx(ebx);
                },
            );
        }
        // Register-addressing coverage: distinct dst/src index pairs (never ECX=1), each dst !=
        // src so the copy is observable, catching a wrong displacement or a dst/src swap that
        // happens to work for the EAX<-EBX case. With `prep`'s seeded flags, this also confirms
        // MOV touches no flag for every index.
        for &(dst, src) in &[(0u8, 7u8), (2, 5), (3, 6), (5, 3), (6, 2), (7, 0)] {
            let op = vec![0x8bu8, 0xc0 | (dst << 3) | src]; // mov <dst>, <src>
            assert_template_identity(
                &op,
                jit::step::SlotKind::RegMov { dst, src },
                &|cpu: &mut Cpu386| {
                    cpu.write_gpr32(dst, 0xaaaa_5555);
                    cpu.write_gpr32(src, 0x1234_5678);
                },
            );
        }
    }

    // ---- Round 3 GATING HARNESS: general multi-iteration + fault-injection + SMC differential ----
    //
    // The one-iteration `assert_template_identity` gate above is structurally blind to the bug
    // classes Round 3's native memory templates introduce: cross-iteration flag/carry propagation
    // over the back-edge, register spill on a mid-loop fault, and self-modifying-code refetch. This
    // harness runs a block SHAPE to completion on the interpreter and the JIT (hotness auto-admit)
    // and asserts full state + memory identity at the halt boundary, with variants that inject a
    // mid-loop fault and a self-store. Every memory template that lands must pass it per shape.
    // Today it validates the TRAMPOLINE (bit-identical), which proves the harness itself is sound.
    // (When native templates make timing approximate, the elapsed_clocks/timing_rem asserts here
    // relax to state-only; the state + memory asserts stay exact - that is the invariant.)

    const H_ENTRY: u32 = 0x101;
    const H_COUNT: usize = 0x400;
    const H_GP_HANDLER: u32 = 0x0b00;

    /// A self-loop `mov al,[esi] ; mov [edi],al ; inc esi ; inc edi ; dec [count] ; jnz` plus a
    /// HLT at the fall-through, a #GP (vector 13) IVT entry to a HLT handler, and `handler` bytes.
    /// A byte-copy loop with a memory load, a memory store, and a memory RMW counter - the exact
    /// operand shapes Round 3 templates target.
    fn h_copy_program() -> Vec<u8> {
        let mut m = vec![0u8; 0x1_0000];
        m[0x100] = 0x90; // nop starter -> H_ENTRY reached as a continuation
        let body: [u8; 13] = [
            0x8a, 0x06, // mov al,[esi]
            0x88, 0x07, // mov [edi],al
            0x46, // inc esi
            0x47, // inc edi
            0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [H_COUNT]
            0x75, // jnz rel8 (rel filled below)
        ];
        m[H_ENTRY as usize..H_ENTRY as usize + body.len()].copy_from_slice(&body);
        let rel_at = H_ENTRY as usize + body.len(); // the rel8 byte
        m[rel_at] = ((H_ENTRY as i32) - (rel_at as i32 + 1)) as i8 as u8;
        m[rel_at + 1] = 0xf4; // hlt at the loop fall-through
        // #GP (vector 13) -> 0:H_GP_HANDLER, a HLT (the fault-injection landing).
        m[13 * 4..13 * 4 + 2].copy_from_slice(&(H_GP_HANDLER as u16).to_le_bytes());
        m[H_GP_HANDLER as usize] = 0xf4;
        m
    }

    /// Run `prog` to a halt on both an interpreter CPU and a hotness-auto-admitting JIT CPU under
    /// `arm`, asserting full guest identity + memory + timing at the halt boundary. `expect_region`
    /// pins that the JIT actually compiled and ran a region (drop it for shapes whose SMC churn may
    /// keep the region cold). Returns the final interpreter CPU so a caller can assert its shape
    /// actually exercised its scenario (the fault fired, SMC churned). Panics on any divergence.
    fn assert_shape_identical(
        prog: Vec<u8>,
        arm: &dyn Fn(&mut Cpu386),
        expect_region: bool,
    ) -> Cpu386 {
        let mut interp = fresh();
        let mut jit_cpu = fresh();
        jit_cpu.set_jit_auto_admit(true);
        let mut bus_i = TestBus::with_memory(prog.clone());
        let mut bus_j = TestBus::with_memory(prog);
        arm(&mut interp);
        arm(&mut jit_cpu);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        assert_state_identical(&interp, &jit_cpu);
        assert_eq!(
            interp.eflags(),
            jit_cpu.eflags(),
            "materialized eflags diverged"
        );
        // Timing is still exact under the trampoline, so assert every accumulator
        // (a divergence names the field). Round 3's cost-fold makes JIT-block timing
        // approximate and relaxes these to drift-tolerant; the state assertion above
        // (which ignores exactly these four fields) stays bit-exact.
        assert_eq!(
            interp.elapsed_clocks, jit_cpu.elapsed_clocks,
            "elapsed_clocks diverged"
        );
        assert_eq!(
            interp.core_clocks_so_far, jit_cpu.core_clocks_so_far,
            "core_clocks_so_far diverged"
        );
        assert_eq!(interp.timing_rem, jit_cpu.timing_rem, "timing_rem diverged");
        assert_eq!(interp.fp_rem, jit_cpu.fp_rem, "fp_rem diverged");
        assert_eq!(bus_i.memory, bus_j.memory, "guest memory diverged");
        assert_eq!(
            interp.perf_counters().jit_region_entries,
            0,
            "the interpreter CPU must never compile"
        );
        if expect_region {
            assert!(
                jit_cpu.perf_counters().jit_region_entries > 0,
                "the JIT must have compiled and run a region"
            );
        }
        interp
    }

    /// Assert two CPUs are STATE-identical, ignoring the four timing accumulators
    /// (`elapsed_clocks`, `core_clocks_so_far`, `timing_rem`, `fp_rem`).
    ///
    /// Under the S2 contract a compiled JIT block leaves guest architectural state
    /// (GPRs, materialized EFLAGS, segments + hidden descriptors, control/system
    /// regs, memory-mapped CPU state) BYTE-IDENTICAL to the interpreter, but its
    /// cycle accounting is only approximate. This is the state-exact half of that
    /// contract: it reuses the derived `PartialEq` by zeroing just the timing
    /// fields on throwaway clones, so it covers every present and future state field
    /// automatically without a hand-maintained list. Timing is asserted separately
    /// by the caller (bit-exact today; drift-tolerant once the cost-fold lands).
    fn assert_state_identical(interp: &Cpu386, jit: &Cpu386) {
        assert!(
            state_eq(interp, jit),
            "architectural state diverged (timing fields ignored)"
        );
    }

    /// Bool core of [`assert_state_identical`], for tests that want to check both
    /// directions without catching a panic.
    fn state_eq(interp: &Cpu386, jit: &Cpu386) -> bool {
        let mut a = interp.clone();
        let mut b = jit.clone();
        for c in [&mut a, &mut b] {
            c.elapsed_clocks = 0;
            c.core_clocks_so_far = 0;
            c.timing_rem = 0;
            c.fp_rem = 0;
        }
        a == b
    }

    /// The state comparator must ignore ONLY the four timing accumulators and still
    /// catch a real architectural divergence. If it silently ignored a state field,
    /// every downstream template differential test would be compromised.
    #[test]
    fn state_comparator_ignores_timing_but_catches_state() {
        let base = fresh();
        let mut timing_only = base.clone();
        timing_only.elapsed_clocks = 12_345;
        timing_only.core_clocks_so_far = 999;
        timing_only.timing_rem = 7;
        timing_only.fp_rem = 3;
        assert!(
            state_eq(&base, &timing_only),
            "a timing-only difference must compare state-identical"
        );
        let mut gpr_diff = base.clone();
        gpr_diff.write_gpr32(0, 0xdead_beef);
        assert!(
            !state_eq(&base, &gpr_diff),
            "a GPR difference must be caught"
        );
    }

    /// Sets eip/esp/esi/edi and a non-trivial incoming flag pattern. The loop count lives in the
    /// program image (at `H_COUNT`), not here.
    fn h_arm(esi: u32, edi: u32) -> impl Fn(&mut Cpu386) {
        move |cpu: &mut Cpu386| {
            cpu.registers.eip = 0x100;
            cpu.registers.set_esp(0x0700);
            cpu.write_gpr32(6, esi); // esi
            cpu.write_gpr32(7, edi); // edi
            // Seed non-trivial flags so a template that wrongly clobbers a preserved flag shows.
            cpu.materialize_flags();
            const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
            cpu.registers.eflags =
                (cpu.registers.eflags & !ARITH) | FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF;
        }
    }

    /// Baseline: a long byte-copy loop (load + store + RMW counter) runs many iterations to halt
    /// identically, and the JIT auto-admits and runs a region. Catches per-iteration divergence
    /// that accumulates over the run (invisible to a single-iteration gate).
    #[test]
    fn harness_multi_iteration_copy_loop_is_identical() {
        let build = |count: u32| -> Vec<u8> {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&count.to_le_bytes());
            m
        };
        // 200 iterations, esi/edi in low RAM well inside the flat 64 KB segment limit.
        assert_shape_identical(build(200), &h_arm(0x2000, 0x3000), true);
    }

    /// Fault injection: a mid-loop memory access runs off the DS limit and #GPs, delivering to the
    /// IVT handler FROM INSIDE THE LIVE REGION. The interpreter and the JIT must fault at the SAME
    /// instruction with identical pushed state - the register file must be committed (not stale) at
    /// the fault, the trap the re-plan's spill-on-every-fault-exit rule guards for the eventual
    /// native templates. The fault MUST land after hotness admission (JIT_HOTNESS_THRESHOLD = 32
    /// iterations) so the JIT's own fault-delivery path is what runs - not the interpreter during
    /// warm-up. `expect_region: true` pins that the region actually admitted and ran before faulting.
    #[test]
    fn harness_mid_loop_fault_delivers_identically() {
        // DS base 0, limit 0x2000. esi=0x1000 (the LOAD stays well inside the limit for the whole
        // run). edi=0x1FC0 advances 1/iteration, so the STORE `mov [edi],al` #GPs when edi first
        // exceeds 0x2000 - at iteration ~66, comfortably past the 32-iteration admission threshold,
        // so the fault is delivered by the running region. count=100 so the loop cannot finish first.
        let prog = {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&100u32.to_le_bytes());
            m
        };
        let arm = move |cpu: &mut Cpu386| {
            h_arm(0x1000, 0x1fc0)(cpu);
            let mut ds = cpu.registers.segment(SegmentIndex::Ds);
            ds.limit = 0x2000;
            cpu.registers.set_segment(SegmentIndex::Ds, ds);
        };
        let interp = assert_shape_identical(prog, &arm, true);
        // Confirm the shape ACTUALLY faulted (else it just ran to the loop-end HLT and tested
        // nothing): the guest must have halted in the #GP handler, far above the loop code.
        assert!(
            interp.registers.eip >= H_GP_HANDLER,
            "the memory access must have #GP'd into the handler, eip={:#x}",
            interp.registers.eip
        );
    }

    /// Self-modifying store: `edi` points at the loop's own first opcode byte and `al` is loaded to
    /// equal that byte, so every iteration stores the SAME value into live code - firing the SMC
    /// watch and forcing a re-decode / region re-admit each iteration without changing behavior.
    /// State must stay identical across the write-then-refetch churn (and it stresses the Round 1
    /// unstamp-reprimes-hotness re-admit fix). The region may stay cold under the churn, so it is
    /// not required to run.
    #[test]
    fn harness_self_modifying_store_stays_identical() {
        let prog = {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&40u32.to_le_bytes());
            m
        };
        // esi points at H_ENTRY (whose byte is 0x8a, the loop's first opcode), so al = 0x8a each
        // iteration; edi ALSO points at H_ENTRY, so the store rewrites that byte with its own value.
        // Both esi and edi advance by 1/iteration (inc), so after the store the pointers move on -
        // only the FIRST iteration self-writes, but the SMC epoch/generation churn it triggers must
        // still leave both CPUs identical. (A fixed-pointer variant lands with the store template.)
        let arm = h_arm(H_ENTRY, H_ENTRY);
        let interp = assert_shape_identical(prog, &arm, false);
        // Confirm the self-store ACTUALLY hit live code and triggered SMC handling (else it wrote
        // only data and tested nothing): some SMC narrow-kill or global-flush must have fired.
        let pc = interp.perf_counters();
        assert!(
            pc.smc_narrow_kills > 0 || pc.decode_inval_smc > 0,
            "the self-store must have triggered the SMC watch (narrow={}, global={})",
            pc.smc_narrow_kills,
            pc.decode_inval_smc
        );
    }

    // ---- Round 3 PAGED differential harness ----
    //
    // The real-mode harness above runs with paging OFF (linear == physical). The Round 3 native
    // memory probe's #1 correctness trap (re-plan trap #1) is that the direct-page cache is
    // PHYSICAL-keyed while the guest address is LINEAR, so in paged mode a probe that indexes the
    // cache with the linear address reads the WRONG physical frame. A harness with an IDENTITY map
    // cannot catch that. This one runs the same byte-copy self-loop in 32-bit protected mode with
    // paging ON and a deliberately NON-IDENTITY linear->physical map, so a linear-indexed probe
    // would diverge. Today (trampoline, memory routed through the interpreter leaf) it is
    // bit-identical incl. timing; it gates the paged probe when that lands. The Doom/Quake anchors
    // run paged (137M page-table walks per Doom timedemo), so this is the mode the probe must win
    // in - the unpaged fast path never runs on them.
    //
    // Physical image (256 KiB): page directory at 0x1000, page table at 0x2000 (PDE[0], covers
    // linear 0..4 MiB), the code frame at 0x8000, the data frame at 0x9000. Linear 0x10000 maps to
    // phys 0x8000 (page index 0x10 vs frame 0x8) and linear 0x30000 to phys 0x9000 (0x30 vs 0x9) -
    // the indices differ, so the map is genuinely non-identity.
    const PG_CODE_LIN: u32 = 0x10000;
    const PG_CODE_PHYS: usize = 0x8000;
    const PG_DATA_LIN: u32 = 0x30000;
    const PG_DATA_PHYS: usize = 0x9000;
    const PG_ENTRY_LIN: u32 = PG_CODE_LIN + 1; // loop head, after the nop starter
    const PG_SRC_LIN: u32 = PG_DATA_LIN; // esi
    const PG_DST_LIN: u32 = PG_DATA_LIN + 0x800; // edi
    const PG_COUNT_LIN: u32 = PG_DATA_LIN + 0x400; // dec dword [PG_COUNT_LIN]

    /// The `h_copy_program` byte-copy self-loop, assembled to run at `PG_CODE_LIN` in 32-bit
    /// protected paged mode with the non-identity map above. `count` seeds the loop counter.
    fn paged_copy_program(count: u32) -> Vec<u8> {
        let mut m = vec![0u8; 0x40000];
        // PDE[0] -> PT at phys 0x2000 (present + rw + user).
        m[0x1000..0x1004].copy_from_slice(&0x0000_2007u32.to_le_bytes());
        // PTE[linear>>12] for the code and data pages (frame + present + rw + user).
        let code_pte = 0x2000 + (PG_CODE_LIN as usize >> 12) * 4;
        m[code_pte..code_pte + 4].copy_from_slice(&((PG_CODE_PHYS as u32) | 0x007).to_le_bytes());
        let data_pte = 0x2000 + (PG_DATA_LIN as usize >> 12) * 4;
        m[data_pte..data_pte + 4].copy_from_slice(&((PG_DATA_PHYS as u32) | 0x007).to_le_bytes());
        // Code at phys 0x8000 (= linear 0x10000): nop starter, then the loop body.
        m[PG_CODE_PHYS] = 0x90; // nop -> PG_ENTRY_LIN reached as a continuation
        let body: [u8; 13] = [
            0x8a, 0x06, // mov al,[esi]
            0x88, 0x07, // mov [edi],al
            0x46, // inc esi
            0x47, // inc edi
            0xff, 0x0d, 0x00, 0x00, 0x00, 0x00, // dec dword [disp32] (disp filled below)
            0x75, // jnz rel8 (rel filled below)
        ];
        let body_at = PG_CODE_PHYS + 1;
        m[body_at..body_at + body.len()].copy_from_slice(&body);
        m[body_at + 8..body_at + 12].copy_from_slice(&PG_COUNT_LIN.to_le_bytes());
        let rel_at = body_at + body.len(); // the rel8 byte
        let after = PG_CODE_LIN as i32 + (rel_at as i32 - PG_CODE_PHYS as i32) + 1;
        m[rel_at] = (PG_ENTRY_LIN as i32 - after) as i8 as u8;
        m[rel_at + 1] = 0xf4; // hlt at the loop fall-through
        let count_phys = PG_DATA_PHYS + (PG_COUNT_LIN - PG_DATA_LIN) as usize;
        m[count_phys..count_phys + 4].copy_from_slice(&count.to_le_bytes());
        m
    }

    /// Arm a CPU for `paged_copy_program`: flat 32-bit protected mode, paging on, CPL 0, esi/edi
    /// at the given linear addresses, and the same non-trivial incoming flags as `h_arm`.
    fn pg_arm(esi: u32, edi: u32) -> impl Fn(&mut Cpu386) {
        move |cpu: &mut Cpu386| {
            let flat = |access: u8| SegmentRegister {
                selector: 0x08,
                base: 0,
                limit: 0xffff_ffff,
                access,
                default_size_32: true,
            };
            cpu.registers.set_segment(SegmentIndex::Cs, flat(0x9b)); // code, exec/read
            cpu.registers.set_segment(SegmentIndex::Ds, flat(0x93)); // data, r/w
            cpu.registers.set_segment(SegmentIndex::Ss, flat(0x93));
            cpu.registers.set_segment(SegmentIndex::Es, flat(0x93));
            cpu.cpl = 0;
            cpu.control.cr3 = 0x1000;
            cpu.control.cr0 |= CR0_PE | CR0_PG;
            cpu.registers.eip = PG_CODE_LIN;
            cpu.registers.set_esp(PG_DATA_LIN + 0xf00); // mapped; the loop never touches it
            cpu.write_gpr32(6, esi); // esi
            cpu.write_gpr32(7, edi); // edi
            cpu.materialize_flags();
            const ARITH: u32 = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
            cpu.registers.eflags =
                (cpu.registers.eflags & !ARITH) | FLAG_CF | FLAG_PF | FLAG_AF | FLAG_SF;
        }
    }

    /// A 200-iteration byte copy under NON-IDENTITY paging stays byte-identical (state + timing)
    /// between the interpreter and the auto-admitting JIT, and the JIT runs a real paged region.
    /// This is the gating harness for the Round 3 paged memory probe: a probe that indexes the
    /// physical page cache with the linear address (trap #1) would read the wrong frame here and
    /// diverge, because the linear page and physical frame indices differ.
    #[test]
    fn harness_paged_copy_loop_is_identical() {
        let interp = assert_shape_identical(
            paged_copy_program(200),
            &pg_arm(PG_SRC_LIN, PG_DST_LIN),
            true,
        );
        assert!(
            interp.is_paging_enabled(),
            "the harness must run with paging enabled"
        );
    }

    /// Self-modifying store under paging: `edi` == `esi` == the loop's own first opcode (linear
    /// PG_ENTRY_LIN), so each iteration reads a code byte and writes it back to the SAME linear
    /// address through the page tables - firing the physical-keyed SMC watch. The re-decode /
    /// region re-admit churn must leave both CPUs identical. The region may stay cold under the
    /// churn, so it is not required to run.
    #[test]
    fn harness_paged_self_modifying_store_stays_identical() {
        let interp = assert_shape_identical(
            paged_copy_program(40),
            &pg_arm(PG_ENTRY_LIN, PG_ENTRY_LIN),
            false,
        );
        let pc = interp.perf_counters();
        assert!(
            pc.smc_narrow_kills > 0 || pc.decode_inval_smc > 0,
            "the self-store must have triggered the SMC watch (narrow={}, global={})",
            pc.smc_narrow_kills,
            pc.decode_inval_smc
        );
    }

    // ---- Round 3 native byte-LOAD probe (isolation test of the emitted assembly) ----

    /// Emit `emit_load_u8_probe` wrapped in a callable prologue/epilogue (pin cpu in R12, regs base
    /// in RBP per current emit_region v3 ABI), run it against the live CPU, and return whether it hit.
    /// On a hit the emitted code has written the loaded byte into `gpr[dst]`'s byte lane.
    fn run_load_probe(cpu: &mut Cpu386, base: u8, index: Option<u8>, disp: i32, dst: u8) -> bool {
        use jit::encoder::{Encoder, Reg};
        let regs_off = std::mem::offset_of!(Cpu386, registers) as u32;
        let mut e = Encoder::new();
        e.push(Reg::RBX);
        e.push(Reg::RBP);
        e.push(Reg::R12);
        e.push(Reg::R13);
        e.push(Reg::R14);
        e.push(Reg::R15);
        #[cfg(windows)]
        e.mov_r64_r64(Reg::R12, Reg::RCX); // win64 arg0 = cpu
        #[cfg(not(windows))]
        e.mov_r64_r64(Reg::R12, Reg::RDI); // sysv arg0 = cpu
        e.mov_r64_r64(Reg::RBP, Reg::R12);
        if regs_off != 0 {
            e.add_r64_imm32(Reg::RBP, regs_off);
        }
        let miss = e.label();
        let done = e.label();
        jit::block::emit_load_u8_probe(&mut e, base, index, disp, dst, miss, false);
        e.mov_r32_imm32(Reg::RAX, 1); // hit: fall through here (gpr already written)
        e.jmp(done);
        e.place(miss);
        e.mov_r32_imm32(Reg::RAX, 0); // miss: nothing written
        e.place(done);
        e.pop(Reg::R15);
        e.pop(Reg::R14);
        e.pop(Reg::R13);
        e.pop(Reg::R12);
        e.pop(Reg::RBP);
        e.pop(Reg::RBX);
        e.ret();
        let bytes = e.finish();
        let buf = jit::exec_mem::ExecutableBuffer::new(&bytes).expect("W^X alloc must succeed");
        let f: extern "C" fn(*mut Cpu386) -> i64 = unsafe { std::mem::transmute(buf.entry_ptr()) };
        f(cpu as *mut Cpu386) != 0
    }

    /// The probe assembly, run in isolation against a real `data_read_pages` entry: it must compute
    /// the effective address for `[reg]` and `[base+index]`, probe the physical-keyed page cache,
    /// deref the host pointer at the in-page offset, and write ONLY the destination byte lane
    /// (write_gpr8 semantics) on a hit - and take the miss path when the page is not cached.
    #[test]
    fn native_load_probe_reads_the_right_byte() {
        let mut page = vec![0u8; 0x1000];
        page[3] = 0xAB;
        page[0x10] = 0xCD;
        let mut cpu = fresh();
        cpu.data_read_pages.insert(izarravm_bus::DirectPage {
            physical_page: 0x5000,
            ptr: page.as_mut_ptr(),
            len: 0x1000,
            writable: false,
        });

        // `mov bl, [eax]` with eax = 0x5003 -> the byte at page offset 3 (0xAB) into BL, EBX's
        // upper three bytes preserved.
        cpu.write_gpr32(0, 0x5003); // eax
        cpu.write_gpr32(3, 0xdead_be00); // ebx (dst BL)
        assert!(
            run_load_probe(&mut cpu, 0, None, 0, 3),
            "must hit the cached page"
        );
        assert_eq!(
            cpu.read_gpr32(3),
            0xdead_beab,
            "BL written from page[3]=0xAB, upper bytes preserved"
        );

        // `mov bl, [eax+ecx]` (SIB scale 1): eax=0x5000, ecx=0x10 -> page offset 0x10 (0xCD).
        cpu.write_gpr32(0, 0x5000);
        cpu.write_gpr32(1, 0x10); // ecx (index)
        cpu.write_gpr32(3, 0x0000_0000);
        assert!(
            run_load_probe(&mut cpu, 0, Some(1), 0, 3),
            "SIB form must hit"
        );
        assert_eq!(cpu.read_gpr32(3), 0x0000_00cd, "BL = page[0x10] = 0xCD");

        // `mov bl, [eax+3]` (displacement, no index): eax=0x5000, disp=3 -> page offset 3 (0xAB).
        // Pins that the `disp != 0` branch adds into the EA register (RAX), not a scratch.
        cpu.write_gpr32(0, 0x5000);
        cpu.write_gpr32(3, 0x0000_0000);
        assert!(
            run_load_probe(&mut cpu, 0, None, 3, 3),
            "disp form must hit"
        );
        assert_eq!(
            cpu.read_gpr32(3),
            0x0000_00ab,
            "BL = page[0x5000+3] = 0xAB via disp"
        );

        // `mov bl, [eax+ecx+3]` (index + displacement): eax=0x5000, ecx=0x0d, disp=3 -> offset 0x10.
        cpu.write_gpr32(0, 0x5000);
        cpu.write_gpr32(1, 0x0d); // ecx
        cpu.write_gpr32(3, 0x0000_0000);
        assert!(
            run_load_probe(&mut cpu, 0, Some(1), 3, 3),
            "index+disp must hit"
        );
        assert_eq!(
            cpu.read_gpr32(3),
            0x0000_00cd,
            "BL = page[0x5000+0x0d+3] = 0xCD"
        );

        // A high byte destination (AH = gpr8 index 4 = byte 1 of EAX): write into bits 8-15.
        cpu.write_gpr32(0, 0x5003); // eax base (also the AH target register)
        assert!(run_load_probe(&mut cpu, 0, None, 0, 4), "must hit");
        // EAX was 0x5003; AH (bits 8-15) becomes 0xAB -> 0x0000_ABxx with the low byte 0x03 kept.
        assert_eq!(
            cpu.read_gpr32(0) & 0xffff,
            0xab03,
            "AH set to 0xAB, AL (0x03) preserved"
        );

        // Miss: an address whose physical page is not cached -> the miss path, gpr untouched.
        cpu.write_gpr32(0, 0x9003); // page 0x9000 not inserted
        cpu.write_gpr32(3, 0x1234_5678);
        assert!(
            !run_load_probe(&mut cpu, 0, None, 0, 3),
            "uncached page must miss"
        );
        assert_eq!(
            cpu.read_gpr32(3),
            0x1234_5678,
            "miss leaves the gpr unchanged"
        );
    }

    // ---- Round 3 byte-LOAD template (stage 1: dispatch removal) ----

    /// The classifier must actually route the loop's `mov al,[esi]` (opcode 0x8A, memory operand)
    /// to `SlotKind::MemLoadU8`, so the specialized `jit_execute_load_u8` runs instead of the full
    /// dispatch. Without this assertion the harness tests would pass even if the classifier never
    /// tagged the load (the trampoline is bit-identical either way), so the template would be dead.
    #[test]
    fn byte_load_slot_is_classified_memloadu8() {
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory({
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
            m
        });
        h_arm(0x2000, 0x3000)(&mut cpu);
        drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
        let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit the copy loop");
        let region = cpu.jit_regions.get_mut(idx).unwrap();
        let load_slots = region
            .ctx
            .slots
            .iter()
            .filter(|s| s.kind == jit::step::SlotKind::MemLoadU8)
            .count();
        assert_eq!(
            load_slots, 1,
            "the `mov al,[esi]` slot must classify as MemLoadU8 (got {load_slots})"
        );
    }

    /// Fault injection ON THE BYTE LOAD itself: `esi` runs off the DS limit so `mov al,[esi]`
    /// #GPs mid-region (before the store), delivering identically on the interpreter and the JIT.
    /// The store-fault variant (`harness_mid_loop_fault_delivers_identically`) never exercises the
    /// LOAD's fault path, which the MemLoadU8 executor now owns. The fault must land after the
    /// 32-iteration admission threshold so the running region delivers it.
    #[test]
    fn byte_load_mid_loop_fault_delivers_identically() {
        let prog = {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&100u32.to_le_bytes());
            m
        };
        // esi=0x1FC0 advances 1/iteration, so the LOAD `mov al,[esi]` #GPs when esi first exceeds
        // 0x2000 (iteration ~65, past the 32 admission threshold). edi=0x1000 keeps the store in
        // limit, so the load is the faulting access. count=100 so the loop cannot finish first.
        let arm = move |cpu: &mut Cpu386| {
            h_arm(0x1fc0, 0x1000)(cpu);
            let mut ds = cpu.registers.segment(SegmentIndex::Ds);
            ds.limit = 0x2000;
            cpu.registers.set_segment(SegmentIndex::Ds, ds);
        };
        let interp = assert_shape_identical(prog, &arm, true);
        assert!(
            interp.registers.eip >= H_GP_HANDLER,
            "the byte load must have #GP'd into the handler, eip={:#x}",
            interp.registers.eip
        );
    }

    // ---- Round 3 byte-STORE template (stage 1: dispatch removal) ----

    /// The classifier must route the loop's `mov [edi],al` (opcode 0x88, memory operand) to
    /// `SlotKind::MemStoreU8`, so `jit_execute_store_u8` runs instead of the full dispatch. This
    /// pins the routing itself is live. The store's FAULT path is exercised in-region by
    /// `harness_mid_loop_fault_delivers_identically` (edi runs off the DS limit, so the faulting
    /// access is the store, past the admission threshold). The SMC (note_code_write) behavior is
    /// inherited STRUCTURALLY, not by a dynamic in-region test: `jit_execute_store_u8`'s only store
    /// is `write_memory_u8`, which runs `note_code_write` unconditionally, so the template cannot
    /// diverge on a code-write regardless of which path executes it;
    /// `harness_self_modifying_store_stays_identical` covers the churn (the region may stay cold).
    #[test]
    fn byte_store_slot_is_classified_memstoreu8() {
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory({
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
            m
        });
        h_arm(0x2000, 0x3000)(&mut cpu);
        drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
        let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit the copy loop");
        let region = cpu.jit_regions.get_mut(idx).unwrap();
        let store_slots = region
            .ctx
            .slots
            .iter()
            .filter(|s| s.kind == jit::step::SlotKind::MemStoreU8)
            .count();
        assert_eq!(
            store_slots, 1,
            "the `mov [edi],al` slot must classify as MemStoreU8 (got {store_slots})"
        );
    }

    // ---- Round 3 sized (word/dword) mem-move template (stage 1: dispatch removal) ----

    /// A dword-copy loop `mov eax,[esi]; mov [edi],eax; add esi,4; add edi,4; dec [cnt]; jnz`, in a
    /// 64 KB image. `H_SIZED_CNT` holds the iteration count.
    fn sized_copy_program(count: u32) -> Vec<u8> {
        const H_SIZED_CNT: usize = 0x400;
        let mut m = vec![0u8; 0x1_0000];
        m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
        let body: [u8; 16] = [
            0x8b, 0x06, // mov eax,[esi]   (MemLoadSized)
            0x89, 0x07, // mov [edi],eax   (MemStoreSized)
            0x83, 0xc6, 0x04, // add esi,4
            0x83, 0xc7, 0x04, // add edi,4
            0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [0x400]
        ];
        m[0x101..0x101 + body.len()].copy_from_slice(&body);
        let rel_at = 0x101 + body.len();
        m[rel_at] = 0x75; // jnz 0x101
        m[rel_at + 1] = ((0x101_i32) - (rel_at as i32 + 2)) as i8 as u8;
        m[rel_at + 2] = 0xf4; // hlt at the fall-through
        m[H_SIZED_CNT..H_SIZED_CNT + 4].copy_from_slice(&count.to_le_bytes());
        m
    }

    fn sized_copy_arm(cpu: &mut Cpu386) {
        cpu.registers.eip = 0x100;
        cpu.registers.set_esp(0x0700);
        cpu.write_gpr32(6, 0x2000); // esi
        cpu.write_gpr32(7, 0x3000); // edi
    }

    /// The classifier must route `mov eax,[esi]` (0x8B mem) to `MemLoadSized` and `mov [edi],eax`
    /// (0x89 mem) to `MemStoreSized` so the specialized sized executors run; the register forms
    /// (0x8B/0x89 mode 3) carry a Reg operand and stay off this path.
    #[test]
    fn sized_mem_moves_are_classified() {
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(sized_copy_program(2));
        sized_copy_arm(&mut cpu);
        drive_to_halt(&mut cpu, &mut bus); // warm the loop's decode lines
        let idx = jit::block::try_admit(&mut cpu, 0x101, true).expect("admit the dword loop");
        let region = cpu.jit_regions.get_mut(idx).unwrap();
        let kinds: Vec<_> = region.ctx.slots.iter().map(|s| s.kind).collect();
        assert!(
            kinds.contains(&jit::step::SlotKind::MemLoadSized),
            "`mov eax,[esi]` must classify as MemLoadSized: {kinds:?}"
        );
        assert!(
            kinds.contains(&jit::step::SlotKind::MemStoreSized),
            "`mov [edi],eax` must classify as MemStoreSized: {kinds:?}"
        );
    }

    /// The dword-copy loop runs many iterations to halt bit-identically with the sized executors
    /// (dword load + dword store), and the JIT auto-admits and runs the region. Pins that the sized
    /// templates reproduce the interpreter's `read_memory_sized`/`write_memory_sized` (dword width,
    /// the alignment/page-cross/SMC behavior) exactly across the run.
    #[test]
    fn sized_mem_moves_run_identically() {
        assert_shape_identical(sized_copy_program(200), &sized_copy_arm, true);
    }

    // ---- Linear-block auto-admission gate ----

    /// Auto-admission (hotness) must REFUSE a linear (non-self-loop) block: it runs once per entry
    /// then returns, so the region prologue/epilogue is pure overhead over the same interpreted
    /// instructions (measured: admitting the hot linear basic blocks was a ~2.9x Doom wall
    /// regression). The forced/test path (`reject_linear = false`) still admits it, and a real
    /// self-loop still admits under the gate. Refusal is always state-correct.
    #[test]
    fn auto_admit_gate_refuses_linear_but_keeps_loops() {
        // A linear block: nop; mov eax,ebx; hlt -> build_block yields a 2-slot non-loop.
        let mut lin = fresh();
        let mut m = vec![0u8; 0x1000];
        m[0x200] = 0x90; // nop
        m[0x201] = 0x8b;
        m[0x202] = 0xc3; // mov eax,ebx
        m[0x203] = 0xf4; // hlt
        let mut bus_l = TestBus::with_memory(m);
        lin.registers.eip = 0x200;
        drive_to_halt(&mut lin, &mut bus_l); // warm 0x200..0x203
        assert!(
            jit::block::try_admit_gated(&mut lin, 0x200, true, true).is_none(),
            "auto-admission must refuse a linear block"
        );
        assert!(
            jit::block::try_admit_gated(&mut lin, 0x200, true, false).is_some(),
            "the forced/test path still admits the same linear block"
        );

        // A self-loop (the byte-copy loop) still admits with the gate on.
        let mut lp = fresh();
        let mut bus_p = TestBus::with_memory({
            let mut mm = h_copy_program();
            mm[H_COUNT..H_COUNT + 4].copy_from_slice(&2u32.to_le_bytes());
            mm
        });
        h_arm(0x2000, 0x3000)(&mut lp);
        drive_to_halt(&mut lp, &mut bus_p);
        let region = jit::block::try_admit_gated(&mut lp, H_ENTRY, true, true);
        assert!(
            region.is_some(),
            "a self-loop must still admit under the linear-block gate"
        );
        assert!(
            lp.jit_regions.get_mut(region.unwrap()).unwrap().is_loop,
            "the admitted region is a self-loop"
        );
    }

    // ---- STAGE 2 FINALE: cost-fold native byte-LOAD, state-only differential ----
    //
    // With `IZARRAVM_JIT_FOLD` on, a fold-eligible `mov r8,[EA]` (unpaged, flat DS, 32-bit) runs as
    // a native page-cache probe + folded bookkeeping instead of a `region_step` call, which makes
    // JIT-block timing APPROXIMATE. So these assert STATE identity (the comparator, ignoring the four
    // timing accumulators) rather than the four-accumulator identity the trampoline tests use. Each
    // real-mode case PROVES it took the native path (`jit_native_load_hits > 0`); the paged case
    // proves the CR0.PG gate keeps the unpaged probe OFF (native hits stay 0) while state stays
    // identical through the trampoline.

    /// `FOLD_TIMING` is a process-global read at region emit time; serialize the fold tests so one
    /// dropping the toggle (below) cannot un-fold another mid-admission. No OTHER default-suite test
    /// is fold-eligible (flat DS + unpaged + Approximate), so this only needs to cover fold tests.
    static FOLD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII: hold the fold lock and turn the toggle ON for the test body; restore OFF on drop (even
    /// on a panic), so the process global returns to its default and other tests are undisturbed.
    struct FoldOn(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl FoldOn {
        fn new() -> Self {
            let g = FOLD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            jit::block::FOLD_TIMING.store(true, std::sync::atomic::Ordering::Relaxed);
            FoldOn(g)
        }
    }
    impl Drop for FoldOn {
        fn drop(&mut self) {
            jit::block::FOLD_TIMING.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// `h_arm` plus a FLAT DS (base already 0 in real mode; force limit to max) so the byte-load is
    /// fold-eligible. This is the "flat real / unreal mode" a DOS extender sets up; without it a
    /// real-mode DS (limit 0xffff) is not flat and the probe is correctly gated off.
    fn flat_ds_arm(esi: u32, edi: u32) -> impl Fn(&mut Cpu386) {
        move |cpu: &mut Cpu386| {
            h_arm(esi, edi)(cpu);
            let mut ds = cpu.registers.segment(SegmentIndex::Ds);
            ds.limit = u32::MAX;
            cpu.registers.set_segment(SegmentIndex::Ds, ds);
        }
    }

    /// Run `prog` to a halt on an interpreter CPU and a fold-on auto-admitting JIT CPU under `arm`,
    /// asserting STATE + materialized eflags + memory identity at the halt boundary (timing is
    /// approximate under the fold, so it is NOT asserted). `expect_native_hits`: `Some(true)` = the
    /// native cost-fold LOAD path MUST have run (real-mode, flat DS); `Some(false)` = it MUST have
    /// stayed off (paged, gated by CR0.PG); `None` = don't assert (SMC churn may keep the region
    /// cold). Both buses hand out direct pages so the page cache is populated and the probe HITs.
    /// Returns the interpreter CPU. Panics on any divergence.
    fn assert_fold_state_identical(
        prog: Vec<u8>,
        arm: &dyn Fn(&mut Cpu386),
        expect_native_hits: Option<bool>,
        expect_store_hits: Option<bool>,
        expect_region: bool,
    ) -> Cpu386 {
        let _fold = FoldOn::new();
        let mut interp = fresh();
        let mut jit_cpu = fresh();
        jit_cpu.set_jit_auto_admit(true);
        let mut bus_i = TestBus::with_memory(prog.clone());
        let mut bus_j = TestBus::with_memory(prog);
        bus_i.direct_pages_enabled = true; // populate the page cache so the native probe HITs
        bus_j.direct_pages_enabled = true;
        arm(&mut interp);
        arm(&mut jit_cpu);
        drive_to_halt(&mut interp, &mut bus_i);
        drive_to_halt(&mut jit_cpu, &mut bus_j);
        // The state-exact half of the S2 contract: architectural state byte-identical, timing not.
        assert_state_identical(&interp, &jit_cpu);
        assert_eq!(
            interp.eflags(),
            jit_cpu.eflags(),
            "materialized eflags diverged (fold on)"
        );
        assert_eq!(
            bus_i.memory, bus_j.memory,
            "guest memory diverged (fold on)"
        );
        assert_eq!(
            interp.perf_counters().jit_region_entries,
            0,
            "the interpreter CPU must never compile"
        );
        assert_eq!(
            interp.perf_counters().jit_native_load_hits,
            0,
            "the interpreter must never run a native fold LOAD slot"
        );
        assert_eq!(
            interp.perf_counters().jit_native_store_hits,
            0,
            "the interpreter must never run a native fold STORE slot"
        );
        if expect_region {
            assert!(
                jit_cpu.perf_counters().jit_region_entries > 0,
                "the JIT must have compiled and run a region"
            );
        }
        let check = |actual: u64, expect: Option<bool>, what: &str| match expect {
            Some(true) => assert!(actual > 0, "the native cost-fold {what} path never ran"),
            Some(false) => {
                assert_eq!(
                    actual, 0,
                    "the native fold {what} path must be gated off here"
                )
            }
            None => {}
        };
        check(
            jit_cpu.perf_counters().jit_native_load_hits,
            expect_native_hits,
            "LOAD",
        );
        check(
            jit_cpu.perf_counters().jit_native_store_hits,
            expect_store_hits,
            "STORE",
        );
        interp
    }

    /// Real-mode, flat DS, unpaged: the byte-copy loop's `mov al,[esi]` runs as the native cost-fold
    /// probe and stays STATE-identical to the interpreter across 200 iterations. Proves the native
    /// path actually ran (`jit_native_load_hits > 0`) — the comparator would instantly catch the
    /// begin_instruction / written_pages / EA / eip bugs the fold spec's five gates guard against.
    #[test]
    fn fold_real_mode_copy_loop_is_state_identical_and_native() {
        let build = |count: u32| -> Vec<u8> {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&count.to_le_bytes());
            m
        };
        // The copy loop folds BOTH the load (`mov al,[esi]`) and the store (`mov [edi],al`).
        assert_fold_state_identical(
            build(200),
            &flat_ds_arm(0x2000, 0x3000),
            Some(true),
            Some(true),
            true,
        );
    }

    /// Base+INDEX load (`mov al,[esi+ecx]`, scale 1) folded natively and STATE-identical. The copy
    /// loop above has no index; the real R_DrawColumn loads are `[esi+ecx]`/`[esi+edx]`, so this
    /// exercises the probe's index-EA path (`add_r32_r32`) in an integrated multi-iteration loop, not
    /// just the isolation test. `ecx` is a fixed in-page offset; `esi` walks within the cached page.
    #[test]
    fn fold_index_load_is_state_identical_and_native() {
        let prog = {
            let mut m = vec![0u8; 0x1_0000];
            m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
            let body: [u8; 14] = [
                0x8a, 0x04, 0x0e, // mov al,[esi+ecx]  (base esi, index ecx, scale 1)
                0x88, 0x07, // mov [edi],al
                0x46, // inc esi
                0x47, // inc edi
                0xff, 0x0d, 0x00, 0x04, 0x00, 0x00, // dec dword [H_COUNT]
                0x75, // jnz rel8
            ];
            m[0x101..0x101 + body.len()].copy_from_slice(&body);
            let rel_at = 0x101 + body.len();
            m[rel_at] = ((0x101i32) - (rel_at as i32 + 1)) as i8 as u8;
            m[rel_at + 1] = 0xf4; // hlt at the fall-through
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&200u32.to_le_bytes());
            m
        };
        let arm = move |cpu: &mut Cpu386| {
            flat_ds_arm(0x2000, 0x3000)(cpu);
            cpu.write_gpr32(1, 0x40); // ecx = a fixed in-page index offset; load reads [esi+0x40]
        };
        // Index load + a plain `mov [edi],al` store both fold.
        assert_fold_state_identical(prog, &arm, Some(true), Some(true), true);
    }

    /// The ALU inline slots (RegMov 0x8B mode3, RegAddImm 0x81/0, RegShrImm 0xC1/5) folded natively
    /// alongside a byte load, STATE-identical. The copy loops above use only single-byte inc/dec
    /// (region_step) — this is the only fold test that exercises the ALU-slot fold path (native op +
    /// flag helper + native fold bookkeeping replacing the region_inline_slot CALL). The drawcolumn's
    /// exact ALU shape; a wrong eip advance, flag helper, or raw_clocks in the fold would diverge.
    #[test]
    fn fold_alu_slots_are_state_identical() {
        let prog = {
            let mut m = vec![0u8; 0x1_0000];
            m[0x100] = 0x90; // nop starter -> 0x101 reached as a continuation
            let mut body: Vec<u8> = vec![
                0x8b, 0xc8, // mov ecx,eax        (RegMov)
                0x81, 0xc1, 0x11, 0x22, 0x00, 0x00, // add ecx,0x2211  (RegAddImm)
                0xc1, 0xe9, 0x01, // shr ecx,1          (RegShrImm)
                0x8a, 0x06, // mov al,[esi]       (MemLoadU8 fold)
                0xff, 0x0d, // dec dword [disp32] ...
            ];
            body.extend_from_slice(&(H_COUNT as u32).to_le_bytes()); // dec dword [H_COUNT]
            body.push(0x75); // jnz rel8
            let jnz_at = 0x101 + body.len() - 1; // linear addr of the jnz opcode
            let after = jnz_at + 2;
            body.push((0x101i32 - after as i32) as i8 as u8);
            m[0x101..0x101 + body.len()].copy_from_slice(&body);
            m[0x101 + body.len()] = 0xf4; // hlt at the loop fall-through
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&200u32.to_le_bytes());
            m
        };
        let arm = move |cpu: &mut Cpu386| {
            flat_ds_arm(0x2000, 0x3000)(cpu);
            cpu.write_gpr32(0, 0x1357); // eax feeds the mov/add/shr chain
        };
        // ALU slots + a load fold; this program has no byte store (the dec is 0xFF, not 0x88).
        assert_fold_state_identical(prog, &arm, Some(true), None, true);
    }

    /// Paged (CR0.PG=1, the Doom/Quake anchor mode) with the paged native probe: linear->physical
    /// via TLB before the physical page-cache probe. The #455 harness uses a NON-IDENTITY map
    /// (lin 0x10000->phys 0x8000 etc) so a linear-as-physical bug would read wrong frames and fail
    /// assert_state_identical instantly. Both LOAD and STORE must hit native and state must match.
    #[test]
    fn fold_paged_copy_loop_is_state_identical_and_native() {
        let interp = assert_fold_state_identical(
            paged_copy_program(200),
            &pg_arm(PG_SRC_LIN, PG_DST_LIN),
            Some(true),
            Some(true),
            true,
        );
        assert!(
            interp.is_paging_enabled(),
            "the paged fold test must run with paging enabled"
        );
    }

    /// Self-modifying store under the fold, flat DS: `esi == edi == the loop's first opcode`, so the
    /// byte read is written back into live code, firing the SMC watch. State must stay identical
    /// across the write/refetch churn. The region may stay cold under the churn, so neither the
    /// region nor a native hit is required — only STATE identity + that the SMC watch fired.
    #[test]
    fn fold_self_modifying_store_stays_state_identical() {
        let prog = {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&40u32.to_le_bytes());
            m
        };
        let interp =
            assert_fold_state_identical(prog, &flat_ds_arm(H_ENTRY, H_ENTRY), None, None, false);
        let pc = interp.perf_counters();
        assert!(
            pc.smc_narrow_kills > 0 || pc.decode_inval_smc > 0,
            "the self-store must have triggered the SMC watch (narrow={}, global={})",
            pc.smc_narrow_kills,
            pc.decode_inval_smc
        );
    }

    /// The STORE fold's writability gate (adversarial-review Finding 1): a `data_write_pages` HIT
    /// proves the physical page was writable via SOME segment, not that the current DS permits
    /// writes. A READ-ONLY flat DS (base 0, limit max, no write bit) passes `jit_segment_flat` but a
    /// store through it must #GP — so the store must NOT fold. Warm+admit with a writable DS (store
    /// folds), then re-admit with DS read-only and confirm the store is gated off. Unpaged 32-bit
    /// protected mode with segments set directly (hidden descriptors, no GDT — the pg_arm pattern).
    #[test]
    fn read_only_ds_gates_the_store_fold_off() {
        let _fold = FoldOn::new();
        let flat = |access: u8| SegmentRegister {
            selector: 0x08,
            base: 0,
            limit: 0xffff_ffff,
            access,
            default_size_32: true,
        };
        let setup = |cpu: &mut Cpu386, ds_access: u8| {
            cpu.registers.set_segment(SegmentIndex::Cs, flat(0x9b)); // exec/read
            cpu.registers.set_segment(SegmentIndex::Ds, flat(ds_access));
            cpu.registers.set_segment(SegmentIndex::Ss, flat(0x93)); // r/w stack
            cpu.registers.set_segment(SegmentIndex::Es, flat(0x93));
            cpu.cpl = 0;
            cpu.control.cr0 |= CR0_PE; // protected, UNPAGED (PG stays clear)
            h_arm(0x2000, 0x3000)(cpu); // eip/esp/esi/edi + flags
        };
        let prog = {
            let mut m = h_copy_program();
            m[H_COUNT..H_COUNT + 4].copy_from_slice(&8u32.to_le_bytes());
            m
        };
        let mut cpu = fresh();
        let mut bus = TestBus::with_memory(prog);
        bus.direct_pages_enabled = true;

        // Writable DS: warm the loop's stores + admit → the store folds (has_native_store).
        setup(&mut cpu, 0x93);
        assert!(cpu.jit_segment_flat(SegmentIndex::Ds));
        assert!(cpu.jit_segment_writable(SegmentIndex::Ds));
        assert!(
            cpu.jit_fold_block_eligible(),
            "unpaged flat pmode is fold-eligible"
        );
        drive_to_halt(&mut cpu, &mut bus);
        let idx = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("admit the copy loop");
        assert!(
            cpu.jit_regions.get_mut(idx).unwrap().has_native_store,
            "a writable DS must fold the store"
        );

        // Read-only flat DS: re-admit reads the current DS and must gate the store off (else it
        // would silently write where the interpreter #GPs). Still flat, so the LOAD still folds.
        setup(&mut cpu, 0x90); // present, dpl0, data, read-only (write bit clear)
        assert!(cpu.jit_segment_flat(SegmentIndex::Ds));
        assert!(!cpu.jit_segment_writable(SegmentIndex::Ds));
        let idx2 = jit::block::try_admit(&mut cpu, H_ENTRY, true).expect("re-admit");
        assert!(
            !cpu.jit_regions.get_mut(idx2).unwrap().has_native_store,
            "a read-only DS must gate the store fold off"
        );
    }
}
