// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! C0 Task L: one register-only guest unit (`add eax, ebx`) hand-built in CLIF at the tail
//! convention, executed against a real `CpuGsw`, and compared against the interpreter running
//! the equivalent guest bytes. State equality is on the amended plan's precisely enumerated
//! fields, compared at dispatcher re-entry after the side exit: EAX, the lazy pending-flag
//! descriptor (ADD's flag effects lowered into the descriptor stores per the base design's
//! rule), and EIP at instruction end. Compile latency around `finalize` is printed as an
//! order-of-magnitude smoke check only.

use super::super::*;

use crate::jit::clif::ClifBackend;
use cranelift_codegen::ir::{
    AbiParam, Function, InstBuilder, MemFlagsData, Signature, UserFuncName, types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

const ENTRY: u32 = 0x500;
const SIDE_EXIT_DISPOSITION: i64 = 0xDEAD;
/// `add eax, ebx` in 32-bit code: 01 D8, two bytes.
const GUEST_BYTES: [u8; 2] = [0x01, 0xd8];

fn flat_cpu() -> CpuGsw {
    let mut cpu = CpuGsw::default();
    cpu.set_mode(GswMode::Gsw586);
    cpu.set_native_backend_enabled(false);
    cpu.control.cr0 |= CR0_PE;
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
    cpu.set_eip(ENTRY);
    cpu
}

/// Build the tail-convention unit: load EAX/EBX through `offset_of!` on the real `CpuGsw`
/// fields (never hardcoded offsets), add, store EAX back, lower ADD's flag effects into the
/// interpreter's lazy pending-flag descriptor, advance EIP past the instruction, and return
/// the side-exit disposition through the adapter.
fn build_add_unit() -> Function {
    let gpr_base = core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, gpr);
    let eax_off = i32::try_from(gpr_base).expect("eax offset fits");
    let ebx_off = i32::try_from(gpr_base + 3 * 4).expect("ebx offset fits");
    let eip_off = i32::try_from(
        core::mem::offset_of!(CpuGsw, registers) + core::mem::offset_of!(Registers, eip),
    )
    .expect("eip offset fits");
    let pf_off = i32::try_from(core::mem::offset_of!(CpuGsw, pending_flags))
        .expect("pending flags offset fits");

    let mut sig = Signature::new(CallConv::Tail);
    sig.params.push(AbiParam::new(types::I64)); // &mut CpuGsw
    sig.returns.push(AbiParam::new(types::I64)); // disposition
    let mut func = Function::with_name_signature(UserFuncName::user(0, 10), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let flags = MemFlagsData::trusted();

    let a = builder.ins().load(types::I32, flags, cpu, eax_off);
    let b = builder.ins().load(types::I32, flags, cpu, ebx_off);
    let result = builder.ins().iadd(a, b);
    builder.ins().store(flags, result, cpu, eax_off);

    // The lazy pending-flag descriptor for a dword ADD, exactly as the interpreter's
    // `alu_add`/`jit_set_pending_add` write it: tag = present | dword width | add op,
    // then a (the destination's prior value), b (the source), and the result.
    let tag = builder.ins().iconst(types::I32, 0x8000_0200);
    builder.ins().store(flags, tag, cpu, pf_off);
    builder.ins().store(flags, a, cpu, pf_off + 4);
    builder.ins().store(flags, b, cpu, pf_off + 8);
    builder.ins().store(flags, result, cpu, pf_off + 12);

    let eip = builder.ins().load(types::I32, flags, cpu, eip_off);
    let eip = builder
        .ins()
        .iadd_imm(eip, i64::from(GUEST_BYTES.len() as u32));
    builder.ins().store(flags, eip, cpu, eip_off);

    let disposition = builder.ins().iconst(types::I64, SIDE_EXIT_DISPOSITION);
    builder.ins().return_(&[disposition]);
    builder.finalize();
    func
}

/// The dispatcher-shaped adapter for this unit: host default convention, one ordinary
/// `call_indirect` into the tail convention, forwarding the unit's disposition.
fn build_unit_adapter(backend: &mut ClifBackend) -> *const u8 {
    let mut tail_sig = Signature::new(CallConv::Tail);
    tail_sig.params.push(AbiParam::new(types::I64));
    tail_sig.returns.push(AbiParam::new(types::I64));
    let mut sig = Signature::new(backend.isa().default_call_conv());
    sig.params.push(AbiParam::new(types::I64)); // &mut CpuGsw
    sig.params.push(AbiParam::new(types::I64)); // unit entry
    sig.returns.push(AbiParam::new(types::I64)); // disposition
    let mut func = Function::with_name_signature(UserFuncName::user(0, 11), sig);
    let mut fbc = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let sig_ref = builder.import_signature(tail_sig);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let cpu = builder.block_params(entry)[0];
    let callee = builder.block_params(entry)[1];
    let call = builder.ins().call_indirect(sig_ref, callee, &[cpu]);
    let disposition = builder.inst_results(call)[0];
    builder.ins().return_(&[disposition]);
    builder.finalize();
    backend
        .finalize(func)
        .expect("adapter compiles with zero relocations")
}

#[test]
fn clif_register_unit_matches_the_interpreter_state() {
    const EAX_IN: u32 = 0x7fff_fff0;
    const EBX_IN: u32 = 0x0000_0021;

    // Interpreter reference: the equivalent guest bytes retired through the real decode and
    // execute path in a flat 32-bit code segment.
    let mut memory = vec![0xf4u8; 0x1000];
    memory[ENTRY as usize..ENTRY as usize + GUEST_BYTES.len()].copy_from_slice(&GUEST_BYTES);
    let mut interpreter = flat_cpu();
    interpreter.registers.set_eax(EAX_IN);
    interpreter.registers.set_ebx(EBX_IN);
    let mut bus = TestBus::with_memory(memory);
    interpreter
        .cycle(&mut bus)
        .expect("interpreter retires add eax, ebx");

    // Native unit: compile (latency printed as an order-of-magnitude smoke check), install,
    // and execute against a real CpuGsw through the dispatcher-shaped adapter.
    let mut backend = ClifBackend::new().expect("pinned host ISA on a supported host");
    let unit_func = build_add_unit();
    let started = std::time::Instant::now();
    let unit = backend
        .finalize(unit_func)
        .expect("unit compiles with zero relocations");
    let compile_ns = started.elapsed().as_nanos();
    println!("clif compile_ns={compile_ns}");
    let adapter_ptr = build_unit_adapter(&mut backend);
    // SAFETY: built at the host default convention with exactly this signature; spans are
    // sealed executable for the backend's lifetime.
    let adapter: extern "C" fn(*mut CpuGsw, *const u8) -> i64 =
        unsafe { std::mem::transmute(adapter_ptr) };

    let mut native = flat_cpu();
    native.registers.set_eax(EAX_IN);
    native.registers.set_ebx(EBX_IN);
    let disposition = adapter(&mut native, unit);

    // Dispatcher re-entry after the side exit: the enumerated equality set.
    assert_eq!(disposition, SIDE_EXIT_DISPOSITION);
    assert_eq!(native.registers.eax(), interpreter.registers.eax());
    assert_eq!(native.registers.eax(), EAX_IN.wrapping_add(EBX_IN));
    assert_eq!(
        native.pending_flags, interpreter.pending_flags,
        "the lowered descriptor stores must equal the interpreter's lazy descriptor"
    );
    assert_eq!(native.eflags(), interpreter.eflags());
    assert_eq!(native.registers.eip, interpreter.registers.eip);
    assert_eq!(native.registers.eip, ENTRY + GUEST_BYTES.len() as u32);
}

use crate::jit::clif::cache::{ClifUnitState, clif_key_for, walk_unit};

fn warm_lines(cpu: &mut CpuGsw, bus: &mut TestBus, starts: &[u32]) {
    for &offset in starts {
        cpu.set_eip(offset);
        cpu.fetch_decoded(bus, offset).expect("warm decode");
    }
    cpu.set_eip(ENTRY);
}

/// C1a growth walker (F-A5): terminates on a Jcc terminal (included), computes the guest
/// byte layout, flags the self-loop, and stops at the first unclassifiable opcode.
#[test]
fn clif_walker_terminates_on_jcc_and_flags_the_self_loop() {
    // inc eax; add eax, ebx; jnz -4 (back to entry): 1 + 2 + 2 bytes.
    let code = [0x40, 0x01, 0xd8, 0x75, 0xfb];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 3]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 3);
    assert_eq!(layout.guest_len, 5);
    assert_eq!(&layout.fetch_lens[..3], &[1, 2, 2]);
    assert!(layout.is_self_loop, "taken target is the unit entry");
    assert!(!layout.has_wide_accesses, "register-only unit");

    // The same body with a forward Jcc is not a self-loop.
    let code = [0x40, 0x01, 0xd8, 0x75, 0x02];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1, ENTRY + 3]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 3);
    assert!(!layout.is_self_loop);
}

/// Q1 stop-growth: the first structurally unclassifiable opcode ends the unit BEFORE it,
/// and a cold line ends it the same way.
#[test]
fn clif_walker_stops_before_an_unclassifiable_opcode() {
    // inc eax; hlt (0xf4 is not classifiable by the Direct classifier).
    let code = [0x40, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY, ENTRY + 1]);
    let layout = walk_unit(&cpu, ENTRY, true).expect("unit layout");
    assert_eq!(layout.instructions, 1);
    assert_eq!(layout.guest_len, 1);
    // An entry that is itself unclassifiable yields no unit at all.
    assert!(walk_unit(&cpu, ENTRY + 1, true).is_none());
}

/// K1-K5: the clif key applies the same static exclusions as direct::key_for.
#[test]
fn clif_key_for_applies_the_direct_static_exclusions() {
    let code = [0x40, 0x75, 0xfd];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
    let mut cpu = flat_cpu();
    let mut bus = TestBus::with_memory(memory);
    warm_lines(&mut cpu, &mut bus, &[ENTRY]);
    let key = clif_key_for(&cpu, ENTRY, true).expect("flat 586 key");
    assert_eq!(key.linear, ENTRY);
    assert_eq!(key.physical, ENTRY);
    assert_eq!(key.mode_key, cpu.jit_mode_key());
    // K2: the 16-bit decode variant never keys.
    assert!(clif_key_for(&cpu, ENTRY, false).is_none());
    // K3: only the Approximate personas key.
    cpu.set_mode(GswMode::Gsw386);
    assert!(clif_key_for(&cpu, ENTRY, true).is_none());
    cpu.set_mode(GswMode::Gsw586);
    // K4: the BIOS F-page window never keys.
    assert!(clif_key_for(&cpu, 0x000f_f000, true).is_none());
    assert!(clif_key_for(&cpu, 0x000f_f3ff, true).is_none());
    // K5: a cold line (no physical) never keys.
    assert!(clif_key_for(&cpu, ENTRY + 0x100, true).is_none());
}

/// Cache admission states mirror the Direct roles: Seen before install, Compiled after,
/// Dormant parks, and install refuses keys not in Seen.
#[test]
fn clif_unit_cache_tracks_seen_compiled_and_dormant() {
    use crate::jit::clif::cache::{ClifUnitCache, ClifUnitDescriptor, ClifUnitKey};
    use crate::jit::code_watch::NativeCodeWatch;
    use crate::jit::direct::{MAX_BLOCK_INSTRUCTIONS, SegmentLayout};
    let mut cache = ClifUnitCache::default();
    let mut watch = NativeCodeWatch::default();
    let key = ClifUnitKey {
        linear: 0x1000,
        physical: 0x1000,
        mode_key: 7,
    };
    let descriptor = ClifUnitDescriptor {
        key,
        guest_len: 3,
        fetch_lens: [0; MAX_BLOCK_INSTRUCTIONS],
        instructions: 2,
        segment_layout: SegmentLayout::capture(&CpuGsw::default(), 0, 0).expect("default layout"),
        memory_cpl3: false,
        has_wide_accesses: false,
        is_self_loop: false,
        entry: 0,
        operands: [0; 2 * MAX_BLOCK_INSTRUCTIONS],
        leading: 1,
        x87_mask: 0,
        cum_raw_before: [0; MAX_BLOCK_INSTRUCTIONS],
        cum_lowered_before: [0; MAX_BLOCK_INSTRUCTIONS],
        raw_clocks_total: 2,
        lowered_total: 1,
        cum_access_before: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        access_total: Default::default(),
        terminal: false,
        disp_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_len: [0; MAX_BLOCK_INSTRUCTIONS],
        imm_extend: [Default::default(); MAX_BLOCK_INSTRUCTIONS],
        lea_mask: 0,
        moffs_mask: 0,
        interp_once: false,
        code_host: 0,
        successors: [None; 2],
    };
    // A stand-in sentinel-descriptor address (any stable nonzero address works for a
    // linkless unit test); fresh cells are sentinel-repointed per the N1a discipline.
    let sentinel_marker = 0u64;
    let sentinel_addr = std::ptr::from_ref(&sentinel_marker) as usize;
    let sentinel_portal = cache.sentinel_portal(sentinel_addr);
    let make_cells = || {
        let cells = [
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
            std::sync::Arc::new(crate::jit::links::LinkCell::new()),
        ];
        for cell in &cells {
            cell.set(sentinel_portal.as_ref());
        }
        cells
    };
    assert!(cache.state(key).is_none());
    // Install without Seen refuses.
    assert!(
        cache
            .install(&mut watch, descriptor.clone(), make_cells(), sentinel_addr)
            .is_none()
    );
    cache.note_seen(key);
    assert_eq!(cache.state(key), Some(ClifUnitState::Seen));
    let index = cache
        .install(&mut watch, descriptor, make_cells(), sentinel_addr)
        .expect("install after Seen");
    assert_eq!(cache.state(key), Some(ClifUnitState::Compiled(index)));
    assert_eq!(cache.unit(index).expect("descriptor").instructions, 2);
    // M5: the installed unit's own guest physical range reads watched immediately, proving
    // the install-time `acquire_range` registration actually happened (design section 5
    // test 3's registration probe), not merely that the check machinery works when a watch
    // happens to exist for some other reason.
    assert!(watch.range_watched(key.physical, 3));
    // A separate Seen key parks Dormant without ever acquiring a registration.
    let dormant_key = ClifUnitKey {
        linear: 0x2000,
        physical: 0x2000,
        mode_key: 7,
    };
    cache.note_seen(dormant_key);
    cache.park_dormant(dormant_key);
    assert_eq!(cache.state(dormant_key), Some(ClifUnitState::Dormant));
    assert!(!watch.range_watched(dormant_key.physical, 3));
    cache.clear(&mut watch);
    assert!(cache.state(key).is_none());
    assert!(cache.state(dormant_key).is_none());
    // Clearing the cache releases the registration too: the shared watch reports the range
    // unwatched again once the unit is gone (no leaked refcount past a wholesale drop).
    assert!(!watch.range_watched(key.physical, 3));
}

/// C1b probe: one tiny lowered unit end to end against the interpreter.
#[test]
fn clif_lowered_probe_minimal() {
    // inc eax; add eax,ebx; jnz +1 (forward, not taken when zf...) then hlt
    let code = [0x40, 0x01, 0xd8, 0x75, 0x01, 0xf4, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut interp = flat_cpu();
    interp.registers.set_eax(5);
    interp.registers.set_ebx(7);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;
    for _ in 0..8 {
        interp.set_eip(ENTRY);
        interp.halted = false;
        loop {
            let outcome = interp
                .run_budgeted(&mut interp_bus, 4096)
                .expect("interp run");
            if outcome.halted {
                break;
            }
        }
    }

    let mut clif = flat_cpu();
    clif.set_clif_backend_enabled(true);
    clif.registers.set_eax(5);
    clif.registers.set_ebx(7);
    let mut clif_bus = TestBus::with_memory(memory.clone());
    clif_bus.direct_pages_enabled = true;
    clif_bus.direct_page_clocks = true;
    for pass in 0..8 {
        clif.set_eip(ENTRY);
        clif.halted = false;
        loop {
            let outcome = clif.run_budgeted(&mut clif_bus, 4096).expect("clif run");
            if outcome.halted {
                break;
            }
        }
        println!(
            "pass {pass}: eip={:#x} eax={:#x} eflags_raw={:#x} pending={:x?} entries={}",
            clif.registers.eip,
            clif.registers.eax(),
            clif.registers.eflags,
            clif.pending_flags,
            clif.jit_clif_counters().entries
        );
    }

    assert!(
        clif.jit_clif_counters().entries > 0,
        "unit never entered: {:#?}",
        clif.jit_clif_counters()
    );
    assert_eq!(clif.registers, interp.registers);
    assert_eq!(clif.eflags(), interp.eflags());
    assert_eq!(clif.pending_flags, interp.pending_flags);
    assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks);
    assert_eq!(
        clif_bus.trace.elapsed_clocks(),
        interp_bus.trace.elapsed_clocks()
    );
}

/// C1b probe: straight-line unit with no terminal, full-state check per pass.
#[test]
fn clif_lowered_probe_straight_line() {
    // inc eax; add eax,ebx; hlt
    let code = [0x40, 0x01, 0xd8, 0xf4];
    let mut memory = vec![0xf4u8; 0x2000];
    memory[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);

    let mut interp = flat_cpu();
    interp.registers.set_eax(5);
    interp.registers.set_ebx(7);
    let mut interp_bus = TestBus::with_memory(memory.clone());
    interp_bus.direct_pages_enabled = true;
    interp_bus.direct_page_clocks = true;

    let mut clif = flat_cpu();
    clif.set_clif_backend_enabled(true);
    clif.registers.set_eax(5);
    clif.registers.set_ebx(7);
    let mut clif_bus = TestBus::with_memory(memory.clone());
    clif_bus.direct_pages_enabled = true;
    clif_bus.direct_page_clocks = true;

    for pass in 0..6 {
        for (cpu, bus) in [(&mut interp, &mut interp_bus), (&mut clif, &mut clif_bus)] {
            cpu.set_eip(ENTRY);
            cpu.halted = false;
            loop {
                let outcome = cpu.run_budgeted(bus, 4096).expect("run");
                if outcome.halted {
                    break;
                }
            }
        }
        assert_eq!(clif.registers, interp.registers, "pass {pass}");
        assert_eq!(clif.pending_flags, interp.pending_flags, "pass {pass}");
        assert_eq!(
            clif.registers.eflags, interp.registers.eflags,
            "pass {pass}"
        );
        assert_eq!(clif.elapsed_clocks, interp.elapsed_clocks, "pass {pass}");
        assert_eq!(
            clif_bus.trace.elapsed_clocks(),
            interp_bus.trace.elapsed_clocks(),
            "pass {pass}"
        );
    }
    assert!(clif.jit_clif_counters().entries > 0);
}
