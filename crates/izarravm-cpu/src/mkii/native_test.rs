// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::jit::exec_mem::ExecutableBuffer;

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
mod session {
    use super::*;

    const SENTINELS: [u64; 3] = [
        0x1357_9bdf_2468_ace0,
        0x2468_ace0_1357_9bdf,
        0xfedc_ba98_7654_3210,
    ];

    struct Probe {
        traces: [Option<Code>; 2],
        dispatcher: *const Code,
        cpu: *mut CpuGsw,
        frame: *mut Frame,
        outer_return: usize,
        outer_stack: [usize; 2],
        restored: [u64; 3],
        helper_stack: usize,
        helpers: usize,
        retired: usize,
        valid: bool,
    }

    unsafe extern "C" fn observe(
        cpu: *mut CpuGsw,
        bus: *mut (),
        frame: *mut Frame,
        _: *const Operation,
    ) -> u32 {
        // SAFETY: this test binds both callbacks to its live Probe.
        let probe = unsafe { &mut *bus.cast::<Probe>() };
        probe.valid &= cpu == probe.cpu && frame == probe.frame;
        let stack: usize;
        unsafe {
            core::arch::asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack, preserves_flags))
        };
        if probe.helpers == 0 {
            probe.helper_stack = stack;
        }
        probe.valid &= stack == probe.helper_stack;
        #[cfg(target_os = "windows")]
        if probe.helpers < 2 {
            probe.valid &= walk(
                probe.traces[probe.helpers % 2].as_ref().unwrap(),
                probe.outer_return,
                probe.outer_stack[0],
            );
        }
        probe.helpers += 1;
        1
    }

    unsafe extern "C" fn resolve(cpu: *mut CpuGsw, bus: *mut (), frame: *mut Frame) -> usize {
        // SAFETY: the dispatcher is the only caller; all trace helper calls returned.
        let probe = unsafe { &mut *bus.cast::<Probe>() };
        probe.valid &= cpu == probe.cpu && frame == probe.frame;
        let finished = (probe.helpers - 1) % 2;
        if probe.helpers <= 2 {
            probe.traces[finished] = None;
            probe.retired += 1;
            #[cfg(target_os = "windows")]
            {
                probe.valid &= walk(
                    unsafe { &*probe.dispatcher },
                    probe.outer_return,
                    probe.outer_stack[0],
                );
            }
            probe.traces[finished] = Some(compile(&[]).unwrap());
        }
        if probe.helpers == 4096 {
            0
        } else {
            probe.traces[probe.helpers % 2].as_ref().unwrap().body_ptr() as usize
        }
    }

    #[cfg(target_os = "windows")]
    fn walk(code: &Code, outer_return: usize, outer_stack: usize) -> bool {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            CONTEXT, RtlCaptureContext, RtlLookupFunctionEntry, RtlVirtualUnwind,
        };
        #[repr(C, align(16))]
        struct AlignedContext(CONTEXT);
        // SAFETY: walk only this live stack and registered code, stopping at our caller.
        unsafe {
            let mut captured: AlignedContext = std::mem::zeroed();
            RtlCaptureContext(&mut captured.0);
            for _ in 0..16 {
                let context = &mut captured.0;
                let mut base = 0;
                let entry = RtlLookupFunctionEntry(context.Rip, &mut base, std::ptr::null_mut());
                if base == code.entry_ptr() as u64 && !entry.is_null() {
                    for &offset in &code.unwind_points {
                        let mut sample = AlignedContext(std::ptr::read(context));
                        sample.0.Rip = base + offset as u64;
                        let mut data = std::ptr::null_mut();
                        let mut establisher = 0;
                        RtlVirtualUnwind(
                            0,
                            base,
                            sample.0.Rip,
                            entry,
                            &mut sample.0,
                            &mut data,
                            &mut establisher,
                            std::ptr::null_mut(),
                        );
                        if sample.0.Rip != outer_return as u64
                            || sample.0.Rsp != outer_stack as u64
                            || [sample.0.Rbx, sample.0.R12, sample.0.R13] != SENTINELS
                        {
                            return false;
                        }
                    }
                    return true;
                }
                if context.Rip == outer_return as u64 || context.Rsp > outer_stack as u64 {
                    return false;
                }
                if entry.is_null() {
                    context.Rip = *(context.Rsp as *const u64);
                    context.Rsp += 8;
                } else {
                    let mut data = std::ptr::null_mut();
                    let mut establisher = 0;
                    RtlVirtualUnwind(
                        0,
                        base,
                        context.Rip,
                        entry,
                        context,
                        &mut data,
                        &mut establisher,
                        std::ptr::null_mut(),
                    );
                }
            }
        }
        false
    }

    #[test]
    fn mkii_session_reuses_one_frame_and_retires_traces_after_tail_transfers() {
        let dispatcher = dispatcher().unwrap();
        for _ in 0..2 {
            let mut cpu = CpuGsw::default();
            let mut bus = crate::tests::TestBus::with_memory(vec![0; 64]);
            let mut frame = Frame::new(&cpu, &mut bus, 0);
            frame.helpers[11] = observe;
            frame.resolve = resolve;
            frame.dispatch = dispatcher.body_ptr() as usize;
            let mut probe = Probe {
                traces: [Some(compile(&[]).unwrap()), Some(compile(&[]).unwrap())],
                dispatcher: &dispatcher,
                cpu: &mut cpu,
                frame: &mut frame,
                outer_return: 0,
                outer_stack: [0; 2],
                restored: [0; 3],
                helper_stack: 0,
                helpers: 0,
                retired: 0,
                valid: true,
            };
            assert!(!dispatcher.unwind_points.is_empty());
            assert_eq!(
                unsafe { &*probe.dispatcher }.body_ptr(),
                dispatcher.body_ptr()
            );
            let mut e = Encoder::new();
            let info = prologue(&mut e);
            for (register, value) in SAVED.into_iter().zip(SENTINELS) {
                e.mov_r64_imm64(register, value);
            }
            e.mov_r64_imm64(Reg::RAX, std::ptr::from_mut(&mut probe) as u64);
            e.store_r64_disp32(
                Reg::RAX,
                std::mem::offset_of!(Probe, outer_stack) as i32,
                Reg::RSP,
            );
            e.mov_r64_imm64(Reg::RAX, dispatcher.entry_ptr() as u64);
            e.call_r64(Reg::RAX);
            let return_offset = e.position();
            e.mov_r64_imm64(Reg::RAX, std::ptr::from_mut(&mut probe) as u64);
            e.store_r64_disp32(
                Reg::RAX,
                (std::mem::offset_of!(Probe, outer_stack) + 8) as i32,
                Reg::RSP,
            );
            for (index, register) in SAVED.into_iter().enumerate() {
                e.store_r64_disp32(
                    Reg::RAX,
                    (std::mem::offset_of!(Probe, restored) + index * 8) as i32,
                    register,
                );
            }
            epilogue(&mut e);
            let outer = ExecutableBuffer::new_with_unwind(&e.finish(), &info).unwrap();
            probe.outer_return = outer.entry_ptr() as usize + return_offset;
            let first = probe.traces[0].as_ref().unwrap().body_ptr() as usize;
            // SAFETY: outer forwards the four arguments and preserves the host ABI.
            let function: unsafe extern "C" fn(*mut CpuGsw, *mut (), *mut Frame, usize) =
                unsafe { std::mem::transmute(outer.entry_ptr()) };
            unsafe {
                function(
                    &mut cpu,
                    std::ptr::from_mut(&mut probe).cast(),
                    &mut frame,
                    first,
                )
            };
            assert!(probe.valid);
            assert_eq!(probe.helpers, 4096);
            assert_eq!(probe.retired, 2);
            assert_eq!(probe.restored, SENTINELS);
            assert_eq!(probe.outer_stack[0], probe.outer_stack[1]);
        }
    }
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
fn caller(target: usize) -> (ExecutableBuffer, usize) {
    let mut e = Encoder::new();
    let info = prologue(&mut e);
    e.mov_r64_imm64(Reg::RAX, target as u64);
    e.call_r64(Reg::RAX);
    let return_offset = e.position();
    e.mov_r32_r32(Reg::RAX, Reg::RAX);
    epilogue(&mut e);
    let code = e.finish();
    let buffer = ExecutableBuffer::new_with_unwind(&code, &info).unwrap();
    (buffer, return_offset)
}

#[cfg(all(
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
#[test]
fn mkii_frame_calls_a_host_helper() {
    extern "C" fn answer() -> u32 {
        42
    }
    let (buffer, _) = caller(answer as *const () as usize);
    // SAFETY: caller emits a no-argument C function returning its helper's u32.
    let function: unsafe extern "C" fn() -> u32 =
        unsafe { std::mem::transmute(buffer.entry_ptr()) };
    assert_eq!(unsafe { function() }, 42);
}

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
#[test]
fn mkii_frame_windows_walk_reaches_both_generated_callers() {
    use std::cell::{Cell, RefCell};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        CONTEXT, RtlCaptureContext, RtlLookupFunctionEntry, RtlVirtualUnwind,
    };
    thread_local! {
        static CAPTURE: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        static EXPECTED: Cell<[usize; 2]> = const { Cell::new([0; 2]) };
        static REGISTERED: Cell<[bool; 2]> = const { Cell::new([false; 2]) };
    }
    #[repr(C, align(16))]
    struct AlignedContext(CONTEXT);
    extern "C" fn capture() -> u32 {
        // SAFETY: the captured context belongs to this live stack. Registered frames use
        // the OS unwinder; a leaf frame contains its return address at RSP.
        unsafe {
            let mut aligned: AlignedContext = std::mem::zeroed();
            let context = &mut aligned.0;
            RtlCaptureContext(context);
            let mut frames = Vec::new();
            let expected = EXPECTED.get();
            let mut registered = [false; 2];
            for _ in 0..32 {
                if context.Rip == 0 {
                    break;
                }
                frames.push(context.Rip as usize);
                let mut base = 0;
                let entry = RtlLookupFunctionEntry(context.Rip, &mut base, std::ptr::null_mut());
                let generated = expected.iter().position(|pc| *pc == context.Rip as usize);
                if let Some(index) = generated {
                    registered[index] = !entry.is_null();
                    if entry.is_null() {
                        break;
                    }
                }
                if entry.is_null() {
                    context.Rip = *(context.Rsp as *const u64);
                    context.Rsp += 8;
                } else {
                    let mut data = std::ptr::null_mut();
                    let mut establisher = 0;
                    RtlVirtualUnwind(
                        0,
                        base,
                        context.Rip,
                        entry,
                        context,
                        &mut data,
                        &mut establisher,
                        std::ptr::null_mut(),
                    );
                }
                if generated == Some(1) {
                    break;
                }
            }
            REGISTERED.set(registered);
            CAPTURE.with_borrow_mut(|stored| *stored = frames);
        }
        17
    }

    let (inner, inner_return) = caller(capture as *const () as usize);
    let (outer, outer_return) = caller(inner.entry_ptr() as usize);
    EXPECTED.set([
        inner.entry_ptr() as usize + inner_return,
        outer.entry_ptr() as usize + outer_return,
    ]);
    // SAFETY: both generated frames implement the same no-argument C signature.
    let function: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(outer.entry_ptr()) };
    assert_eq!(unsafe { function() }, 17);
    assert_eq!(REGISTERED.get(), [true, true]);
    CAPTURE.with_borrow(|frames| {
        assert!(
            frames.contains(&(inner.entry_ptr() as usize + inner_return)),
            "{frames:x?}"
        );
        assert!(
            frames.contains(&(outer.entry_ptr() as usize + outer_return)),
            "{frames:x?}"
        );
    });
}
