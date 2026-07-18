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
